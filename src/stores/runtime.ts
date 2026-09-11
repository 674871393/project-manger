import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import type { Project, RunStatus } from "@/types";
import { useProjectsStore } from "@/stores/projects";

/** 单条日志（stdout/stderr 合并流，保留来源用于着色） */
export interface LogLine {
  stream: "stdout" | "stderr";
  line: string;
  /** "install"：依赖安装输出（FR-11），渲染时加前缀区分 */
  tag?: "install";
  /** 历史日志：上次会话落盘读回（FR-10），置灰渲染 */
  historical?: boolean;
}

/** 依赖安装状态（FR-11，内存不持久化，随 install 进程事件更新） */
export interface InstallState {
  status: "running" | "success" | "error" | "stopped";
  exitCode?: number;
  /** 安装成功后自动启动项目（「安装并启动」确认用） */
  thenStart: boolean;
}

/** 手动终止安装的项目（时间无关）：install 退出事件据此跳过「安装失败」误报 */
const manualInstallStops = new Set<string>();

/** 每个项目的运行时状态（内存，不持久化） */
export interface ProjectRuntime {
  status: RunStatus;
  pid?: number;
  url?: string;
  startedAt?: number;
  exitCode?: number;
}

/** 日志环形缓冲上限（PRD FR-4） */
const MAX_LOG_LINES = 2000;

/**
 * 从日志行解析本地地址（PRD FR-5）。
 * 两种形态：
 * 1. 完整 URL：Vite `Local:   http://localhost:5173/`、webpack `App running at: ...`、
 *    Next `- Local:  http://localhost:3000`
 * 2. 纯端口：Vite7 精简输出 `Port: 5173`、`listening on 3000` 等（无 http:// 前缀）
 */
/**
 * 剥掉 ANSI 转义序列。Vite 7/8 打包的 picocolors 在 Windows 上无条件启用颜色
 * （判定条件含 platform === "win32"，即使 stdout 是管道也照发），Local 行实际是
 * `➜  Local:   \x1b[36mhttp://localhost:\x1b[1m5173\x1b[22m/\x1b[39m`——
 * 端口号前插着加粗码，不剥掉的话 URL_RE 永远匹配不上。
 */
const ANSI_RE = /\x1b\[[0-9;]*[A-Za-z]/g;

const URL_RE = /https?:\/\/(?:localhost|127\.0\.0\.1|\[::1\]):(\d{2,5})/;
const PORT_RE = /(?:port|端口)\D{0,8}?(\d{2,5})\b|listening on (\d{2,5})/i;

/** 解析一行日志（须已剥 ANSI），返回端口号（无则 null） */
function parsePort(line: string): number | null {
  const m1 = line.match(URL_RE);
  if (m1) return Number(m1[1]);
  // `Port 5173 is in use, trying another one...` 里的数字是被占用的旧端口，
  // 不是本项目实际监听的端口，拿去探测会误报（占用者恰恰在监听）
  if (/in use|trying another|被占用/i.test(line)) return null;
  const m2 = line.match(PORT_RE);
  if (m2) return Number(m2[1] ?? m2[2]);
  return null;
}

/** 已确认过的地址就不再重复探测（同一端口只探一次） */
const confirmedUrls = new Set<string>();

/**
 * projectId -> 最近一次解析出的运行地址（localStorage 持久化）。
 * 页面刷新（F5 / dev 热重载）后日志缓冲清空、旧日志不会重发，
 * url 无从再解析，靠这份缓存恢复地址列展示。
 */
const URL_CACHE_KEY = "fpm:last-urls";

function loadCachedUrls(): Record<string, string> {
  try {
    const parsed = JSON.parse(localStorage.getItem(URL_CACHE_KEY) ?? "{}");
    return typeof parsed === "object" && parsed !== null ? parsed : {};
  } catch {
    return {};
  }
}

function cacheUrl(projectId: string, url: string) {
  try {
    const all = loadCachedUrls();
    all[projectId] = url;
    localStorage.setItem(URL_CACHE_KEY, JSON.stringify(all));
  } catch {
    // localStorage 不可用（隐私模式等）：仅影响刷新后的地址回显，不致命
  }
}

/** 进程退出/手动停止后调用：下次启动时重新解析端口 */
function resetConfirmedUrls(projectId: string) {
  for (const key of confirmedUrls) {
    if (key.startsWith(`${projectId}:`)) confirmedUrls.delete(key);
  }
}

/** TCP 连通即认为服务就绪（前端 fetch 会受 CORS 限制，探 TCP 不受影响）。
 * 服务可能「打印地址」早于「真正监听」，失败后重试几次而不是一次放弃。 */
async function probePort(port: number, attempts = 3): Promise<boolean> {
  for (let i = 0; i < attempts; i++) {
    try {
      await invoke("probe_port", { port, timeoutMs: 1500 });
      return true;
    } catch {
      await new Promise((r) => setTimeout(r, 1000));
    }
  }
  return false;
}

/** stop/restart 用：等 process-exit 事件到达再继续（超时兜底） */
const exitWaiters = new Map<string, () => void>();

/**
 * 主动停止的宽限标记（时间戳）。stop 超时兜底（进程卡死）后 waiters 已被清掉，
 * 迟到的 process-exit 事件会误判为崩溃弹日志抽屉——在宽限期内视为主动停止。
 */
const intentionalStops = new Map<string, number>();
const INTENTIONAL_GRACE_MS = 15_000;

function waitForExit(projectId: string, timeoutMs: number): Promise<boolean> {
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      exitWaiters.delete(projectId);
      resolve(false); // 超时：进程没退出，调用方据此处理
    }, timeoutMs);
    exitWaiters.set(projectId, () => {
      clearTimeout(timer);
      resolve(true);
    });
  });
}

/** initListeners 幂等保护：React StrictMode(dev) 下 effect 双执行会重复挂监听，
 * 同一个 process-exit 被处理两次，第二次因 waiter 已被消费而误判为崩溃弹日志 */
let listenersReady = false;

interface RuntimeStore {
  runtimes: Record<string, ProjectRuntime>;
  logs: Record<string, LogLine[]>;
  /** 本机局域网 IP（展示用；null = 未知，回退 localhost） */
  lanIp: string | null;
  /** error 时自动弹日志抽屉（PRD 4.3），App 层消费后清空 */
  pendingErrorProjectId: string | null;
  /** 依赖安装状态（FR-11） */
  installs: Record<string, InstallState>;
  /** 启动时发现缺 node_modules 的项目（FR-11 预检），App 层确认弹窗消费 */
  pendingInstall: Project[];

  /** 挂一次全局事件监听（App 挂载时调用） */
  initListeners: () => Promise<void>;
  /** 从 Rust pid 表同步运行状态（页面重载后找回真实状态） */
  syncFromBackend: () => Promise<void>;
  loadLanIp: () => Promise<void>;
  clearLogs: (projectId: string) => void;
  /** 启动项目；返回是否真正下发启动（false = 已在运行或转入了安装确认流程） */
  start: (project: Project) => Promise<boolean>;
  stop: (project: Project) => Promise<void>;
  restart: (project: Project) => Promise<void>;
  /** 安装依赖（FR-11）；thenStart = 安装成功后自动启动 */
  install: (project: Project, opts?: { thenStart?: boolean }) => Promise<void>;
  /** 终止依赖安装（安装卡死时的手动兜底） */
  stopInstall: (project: Project) => Promise<void>;
  /** 处理「未安装依赖，是否安装并启动？」的确认结果（App 层弹窗调用） */
  resolvePendingInstall: (confirmed: boolean) => void;
}

export const useRuntimeStore = create<RuntimeStore>((set, get) => ({
  runtimes: {},
  logs: {},
  lanIp: null,
  pendingErrorProjectId: null,
  installs: {},
  pendingInstall: [],

  initListeners: async () => {
    if (listenersReady) return;
    listenersReady = true;

    // 孤儿进程清理结果提示（Rust setup 阶段发的事件，可能早于本监听注册——
    // 事件监听慢于 setup 只能尽力而为，清理本身在 Rust 侧已完成，这里只是提示）
    await listen<number>("orphans-cleaned", (ev) => {
      toast.info(`检测到上次异常退出，已清理 ${ev.payload} 个残留进程`);
    });

    await listen<{
      projectId: string;
      stream: "stdout" | "stderr";
      kind?: string;
      line: string;
    }>("log-line", (ev) => {
      const { projectId, stream, line: raw } = ev.payload;
      const tag: "install" | undefined =
        ev.payload.kind === "install" ? "install" : undefined;
      // 剥 ANSI 颜色码：日志面板按纯文本渲染（不剥会显示 [36m 这类乱码），
      // 端口解析也依赖干净文本（见 ANSI_RE 注释）
      const line = raw.replace(ANSI_RE, "");
      set((s) => {
        const buf = [...(s.logs[projectId] ?? []), { stream, line, tag }];
        if (buf.length > MAX_LOG_LINES) buf.splice(0, buf.length - MAX_LOG_LINES);
        return { logs: { ...s.logs, [projectId]: buf } };
      });

      // 端口解析：starting/running 期间匹配到端口就探测并更新（PRD FR-5）
      const rt = get().runtimes[projectId];
      if (rt && (rt.status === "starting" || rt.status === "running")) {
        const port = parsePort(line);
        if (port !== null) {
          const key = `${projectId}:${port}`;
          if (!confirmedUrls.has(key)) {
            confirmedUrls.add(key);
            probePort(port).then((ok) => {
              if (!ok) return;
              const url = `http://localhost:${port}`;
              cacheUrl(projectId, url);
              set((s) => {
                const cur = s.runtimes[projectId];
                if (!cur || cur.status === "stopped") return {};
                return {
                  runtimes: {
                    ...s.runtimes,
                    [projectId]: {
                      ...cur,
                      url,
                      status: "running",
                    },
                  },
                };
              });
            });
          }
        }
      }
    });

    await listen<{ projectId: string; code: number | null; kind?: string }>(
      "process-exit",
      (ev) => {
        const { projectId, code } = ev.payload;

        // 依赖安装进程退出（FR-11）：更新 installs 状态；
        // 成功且带 thenStart → 接着启动项目；失败 → toast + 自动弹日志抽屉
        if (ev.payload.kind === "install") {
          const st = get().installs[projectId];
          if (st && st.status === "running") {
            const manual = manualInstallStops.has(projectId);
            manualInstallStops.delete(projectId);
            const success = !manual && code === 0;
            set((s) => ({
              installs: {
                ...s.installs,
                [projectId]: {
                  status: manual ? "stopped" : success ? "success" : "error",
                  exitCode: code ?? undefined,
                  thenStart: false,
                },
              },
            }));
            const project = useProjectsStore
              .getState()
              .projects.find((p) => p.id === projectId);
            if (project) {
              if (manual) {
                toast.info(`已终止「${project.name}」依赖安装`);
              } else if (success) {
                toast.success(`「${project.name}」依赖安装完成`);
                if (st.thenStart) void get().start(project);
              } else {
                toast.error(
                  `「${project.name}」依赖安装失败（退出码 ${code ?? "未知"}），详见日志`,
                );
                set({ pendingErrorProjectId: projectId });
              }
            }
          }
          return;
        }

        // 有 waiter / 宽限标记 = 我们主动 stop 的，属正常退出；否则是崩溃/被外部杀死 → error
        const waiter = exitWaiters.get(projectId);
        const grace = intentionalStops.get(projectId);
        const intentional =
          waiter !== undefined ||
          (grace !== undefined && Date.now() - grace < INTENTIONAL_GRACE_MS);
        if (waiter) {
          exitWaiters.delete(projectId);
          waiter();
        }
        if (grace !== undefined) intentionalStops.delete(projectId);
        resetConfirmedUrls(projectId);
        set((s) => ({
          runtimes: {
            ...s.runtimes,
            [projectId]: {
              status: intentional ? "stopped" : "error",
              exitCode: code ?? undefined,
            },
          },
          // 非主动退出（崩溃/被外部杀）→ 自动弹日志抽屉（PRD 4.3 / 验收 4）
          pendingErrorProjectId: intentional ? s.pendingErrorProjectId : projectId,
        }));
      },
    );
  },

  syncFromBackend: async () => {
    try {
      const running = await invoke<Record<string, number>>("get_running");
      const cachedUrls = loadCachedUrls();
      set((s) => {
        const runtimes = { ...s.runtimes };
        // Rust 说在跑 → 标 running（url 从缓存恢复，日志已丢无法再解析）；Rust 说没跑但前端显示 running → 拉回 stopped
        for (const [id, pid] of Object.entries(running)) {
          const cur = runtimes[id];
          if (!cur || cur.status === "stopped" || cur.status === "error") {
            runtimes[id] = {
              status: "running",
              pid,
              startedAt: cur?.startedAt,
              url: cachedUrls[id],
            };
          }
        }
        for (const [id, rt] of Object.entries(runtimes)) {
          if (rt.status === "running" && !(id in running)) {
            runtimes[id] = { status: "stopped" };
          }
        }
        return { runtimes };
      });
    } catch {
      // 命令不可用（异常场景）：保持现状
    }
  },

  loadLanIp: async () => {
    try {
      const ip = await invoke<string>("get_lan_ip");
      set({ lanIp: ip });
    } catch {
      // 无网络/未知环境：保持 null，地址列回退 localhost
    }
  },

  clearLogs: (projectId) => set((s) => ({ logs: { ...s.logs, [projectId]: [] } })),

  start: async (project) => {
    const rt = get().runtimes[project.id];
    if (rt && (rt.status === "running" || rt.status === "starting")) return false;

    // 依赖预检（FR-11）：node_modules 缺失 → 转入「安装并启动」确认流程（App 层弹窗消费），
    // 不直接启动——直接跑只会看到一屏 module not found 报错
    try {
      const hasDeps = await invoke<boolean>("check_deps", { path: project.path });
      if (!hasDeps) {
        set((s) => ({
          pendingInstall: [
            ...s.pendingInstall.filter((p) => p.id !== project.id),
            project,
          ],
        }));
        return false;
      }
    } catch {
      // 预检本身失败不阻塞：按原流程启动，真有问题走 error 流程
    }

    // 端口冲突预警（PRD FR-5 P1）：配置了手动端口且已被占用 → 警告但不阻止
    // （脚手架可能自动换端口，实际以日志解析为准）
    if (project.manualPort) {
      try {
        await invoke("probe_port", { port: project.manualPort, timeoutMs: 800 });
        toast.warning(
          `端口 ${project.manualPort} 已被占用，「${project.name}」可能启动失败或自动换端口`,
        );
      } catch {
        // 连不通 = 端口空闲，符合预期
      }
    }

    set((s) => ({
      runtimes: {
        ...s.runtimes,
        [project.id]: { status: "starting", startedAt: Date.now() },
      },
    }));
    try {
      const pid = await invoke<number>("start_project", { project });
      set((s) => {
        const cur = s.runtimes[project.id];
        // 脚本报错时进程可能瞬间退出、exit 事件先于 invoke 返回到达，别覆盖 error
        if (cur && cur.status !== "starting") return {};
        return {
          runtimes: {
            ...s.runtimes,
            [project.id]: { status: "running", pid, startedAt: Date.now() },
          },
        };
      });
      return true;
    } catch (e) {
      // 前端状态与 Rust pid 表失同步（页面重载后常见）：同步真实状态再展示
      await get().syncFromBackend();
      const stillRunning =
        get().runtimes[project.id]?.status === "running";
      set((s) => ({
        runtimes: {
          ...s.runtimes,
          [project.id]: stillRunning
            ? s.runtimes[project.id]
            : { status: "stopped" },
        },
      }));
      if (!stillRunning) {
        toast.error(`启动「${project.name}」失败: ${e}`);
      }
      return stillRunning;
    }
  },

  stop: async (project) => {
    const rt = get().runtimes[project.id];
    if (!rt || rt.status !== "running") return;
    try {
      // 先注册 exit waiter 再下发 kill：taskkill /F 是毫秒级，
      // exit 事件可能抢在 invoke 返回前到达，先挂 waiter 才不会被误判为崩溃
      const exited = waitForExit(project.id, 8000);
      await invoke("stop_project", { projectId: project.id });
      const ok = await exited;
      const cur = get().runtimes[project.id];
      if (ok || cur?.status === "running") {
        // 正常等到退出事件，或超时兜底（进程卡死也按已停处理，重启路径才能走下去）。
        // 超时路径打宽限标记：之后迟到的 exit 事件不再误判为崩溃弹日志
        if (!ok) intentionalStops.set(project.id, Date.now());
        set((s) => ({
          runtimes: { ...s.runtimes, [project.id]: { status: "stopped" } },
        }));
      }
    } catch (e) {
      toast.error(`停止「${project.name}」失败: ${e}`);
    }
  },

  // 先完整走停止、确认退出后再启动（PRD FR-3：不并行发两份命令）
  restart: async (project) => {
    await get().stop(project);
    await get().start(project);
  },

  install: async (project, opts) => {
    if (get().installs[project.id]?.status === "running") return;
    set((s) => ({
      installs: {
        ...s.installs,
        [project.id]: { status: "running", thenStart: opts?.thenStart ?? false },
      },
    }));
    try {
      await invoke<number>("install_project", { project });
    } catch (e) {
      set((s) => ({
        installs: {
          ...s.installs,
          [project.id]: { status: "error", thenStart: false },
        },
      }));
      toast.error(`安装「${project.name}」依赖失败: ${e}`);
    }
  },

  resolvePendingInstall: (confirmed) => {
    const pending = get().pendingInstall;
    set({ pendingInstall: [] });
    if (confirmed) {
      for (const p of pending) void get().install(p, { thenStart: true });
    }
  },

  stopInstall: async (project) => {
    manualInstallStops.add(project.id);
    try {
      await invoke("stop_install", { projectId: project.id });
    } catch (e) {
      manualInstallStops.delete(project.id);
      toast.error(`终止「${project.name}」依赖安装失败: ${e}`);
    }
  },
}));
