import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import type { Project, ReleaseAnnouncement, RunStatus } from "@/types";
import { useProjectsStore } from "@/stores/projects";
import { parseDevelopmentAddress, portAddress, stripAnsi, type AddressCandidate } from "@/lib/project-address";

/** 单条日志（stdout/stderr 合并流，保留来源用于着色） */
export interface LogLine {
  stream: "stdout" | "stderr";
  line: string;
  /** 非来源输出：依赖安装（install）/ 一键发布（release），渲染时加前缀区分 */
  tag?: "install" | "release";
  /** 历史日志：上次会话落盘读回（FR-10），置灰渲染 */
  historical?: boolean;
}

/** 一键发布流水线的步骤（PRD FR-20，与 Rust release.rs 对应） */
export type ReleaseStep =
  | "precheck"
  | "checkout"
  | "node"
  | "bump"
  | "build"
  | "zip";

export type ReleaseStepStatus = "running" | "success" | "error";

/** 每个项目的发布运行状态（内存，随 release-step/release-finished 事件更新） */
export interface ReleaseRun {
  running: boolean;
  steps: Partial<Record<ReleaseStep, ReleaseStepStatus>>;
  /** 步骤附注（版本号变化、zip 路径等） */
  messages: Partial<Record<ReleaseStep, string>>;
  zipPath?: string;
  error?: string;
}

/** 依赖安装状态（FR-11，内存不持久化，随 install 进程事件更新） */
export interface InstallState {
  status: "running" | "success" | "error" | "stopped";
  exitCode?: number;
  /** 安装成功后自动启动项目（「安装并启动」确认用） */
  thenStart: boolean;
  runId?: string;
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
  runId?: string;
  address?: AddressCandidate;
  lanReachable?: boolean;
}

/** 日志环形缓冲上限（PRD FR-4） */
const MAX_LOG_LINES = 2000;


export type StartResult = "started" | "alreadyRunning" | "needsDependencies" | "failed";

export function isActive(status?: RunStatus): boolean {
  return status === "starting" || status === "running" || status === "ready";
}

const startingProjects = new Map<string, Promise<StartResult>>();
const stoppingProjects = new Map<string, Promise<boolean>>();
const startingInstalls = new Map<string, Promise<void>>();
const stoppingInstalls = new Map<string, Promise<boolean>>();
const manualStops = new Set<string>();
const probing = new Set<string>();
const addressCachePrefix = "fpm:address:";
let listenersReady: Promise<void> | undefined;

function setRuntime(projectId: string, runtime: ProjectRuntime) {
  useRuntimeStore.setState((state) => ({
    runtimes: { ...state.runtimes, [projectId]: runtime },
  }));
}

function readCachedAddress(runId: string): AddressCandidate | undefined {
  try {
    return JSON.parse(sessionStorage.getItem(addressCachePrefix + runId) ?? "null") ?? undefined;
  } catch {
    return undefined;
  }
}

function clearCachedAddress(runId: string) {
  try {
    sessionStorage.removeItem(addressCachePrefix + runId);
  } catch {
    // 存储不可用时仍可从日志识别可用地址。
  }
}

function canConfirmAddress(projectId: string, runId: string, address: AddressCandidate) {
  const current = useRuntimeStore.getState().runtimes[projectId];
  if (current?.runId !== runId || !isActive(current.status)) return;
  if (current.address && current.address.priority >= address.priority) return;
  return current;
}

async function trackAddress(projectId: string, runId: string, address: AddressCandidate) {
  const key = runId + "/" + address.priority + "/" + address.url;
  if (probing.has(key)) return;
  probing.add(key);
  try {
    const attempts = address.priority === 0 ? 30 : 8;
    for (let attempt = 0; attempt < attempts; attempt++) {
      if (!canConfirmAddress(projectId, runId, address)) return;
      try {
        const lanReachable = await invoke<boolean>("probe_project_port", {
          projectId, runId, port: address.port, host: address.host,
        });
        const current = canConfirmAddress(projectId, runId, address);
        if (!current) return;
        setRuntime(projectId, { ...current, status: "ready", url: address.url, address, lanReachable });
        try {
          sessionStorage.setItem(addressCachePrefix + runId, JSON.stringify(address));
        } catch {
          // 无法缓存时不影响本次运行。
        }
        return;
      } catch {
        if (attempt + 1 < attempts) await new Promise((resolve) => setTimeout(resolve, 650));
      }
    }
  } finally {
    probing.delete(key);
  }
}

interface ActiveProcess {
  pid: number;
  runId: string;
}

interface RunningProcesses {
  runs: Record<string, ActiveProcess>;
  installs: Record<string, ActiveProcess>;
}

interface RuntimeStore {
  runtimes: Record<string, ProjectRuntime>;
  removingProjects: Set<string>;
  removeProjects: (projects: Project[]) => Promise<{ removed: Project[]; failed: Project[] }>;
  logs: Record<string, LogLine[]>;
  /** 本机局域网 IP（展示用；null = 未知，回退 localhost） */
  lanIp: string | null;
  /** error 时自动弹日志抽屉（PRD 4.3），App 层消费后清空 */
  pendingErrorProjectId: string | null;
  /** 依赖安装状态（FR-11） */
  installs: Record<string, InstallState>;
  /** 启动时发现缺 node_modules 的项目（FR-11 预检），App 层确认弹窗消费 */
  pendingInstall: Project[];
  /** 发布流水线运行状态（FR-20） */
  releaseRuns: Record<string, ReleaseRun>;

  /** 挂一次全局事件监听（App 挂载时调用） */
  initListeners: () => Promise<void>;
  /** 从 Rust pid 表同步运行状态（页面重载后找回真实状态） */
  syncFromBackend: () => Promise<void>;
  loadLanIp: () => Promise<void>;
  clearLogs: (projectId: string) => void;
  /** 启动进程，调用方据结果反馈已运行、需要安装或失败。 */
  start: (project: Project) => Promise<StartResult>;
  stop: (project: Project) => Promise<boolean>;
  restart: (project: Project) => Promise<StartResult>;
  /** 安装依赖（FR-11）；thenStart = 安装成功后自动启动 */
  install: (project: Project, opts?: { thenStart?: boolean }) => Promise<void>;
  /** 终止依赖安装（安装卡死时的手动兜底） */
  stopInstall: (project: Project) => Promise<boolean>;
  /** 处理「未安装依赖，是否安装并启动？」的确认结果（App 层弹窗调用） */
  resolvePendingInstall: (confirmed: boolean) => void;
  /** 启动一键发布流水线（FR-20）：进度走 release-step 事件。
   * expectedVersion 是弹窗确认过的版本基准，Rust 在 checkout 后据此校验是否已变化 */
  startRelease: (
    project: Project,
    newVersion: string,
    expectedVersion: string,
    announcement?: ReleaseAnnouncement,
  ) => Promise<void>;
}

export const useRuntimeStore = create<RuntimeStore>((set, get) => ({
  runtimes: {},
  removingProjects: new Set(),
  logs: {},
  lanIp: null,
  pendingErrorProjectId: null,
  installs: {},
  pendingInstall: [],
  releaseRuns: {},

  initListeners: () => {
    if (listenersReady) return listenersReady;
    listenersReady = (async () => {

      // 孤儿进程清理结果提示（Rust setup 阶段发的事件，可能早于本监听注册——
      // 事件监听慢于 setup 只能尽力而为，清理本身在 Rust 侧已完成，这里只是提示）
      await listen<number>("orphans-cleaned", (ev) => {
        toast.info(`检测到上次异常退出，已清理 ${ev.payload} 个残留进程`);
      });
      await listen("release-exit-blocked", () => toast.info("正在发布，请等待任务完成后退出。"));

      await listen<{
        projectId: string;
        stream: "stdout" | "stderr";
        kind: "run" | "install" | "release";
        runId: string | null;
        line: string;
      }>("log-line", (ev) => {
        const { projectId, stream, line: raw } = ev.payload;
        const tag: "install" | "release" | undefined =
          ev.payload.kind === "install" || ev.payload.kind === "release"
            ? ev.payload.kind
            : undefined;
        // 剥 ANSI 颜色码：日志面板按纯文本渲染（不剥会显示 [36m 这类乱码），
        // 端口解析也依赖干净文本（见 ANSI_RE 注释）
        const line = stripAnsi(raw);
        set((s) => {
          const buf = [...(s.logs[projectId] ?? []), { stream, line, tag }];
          if (buf.length > MAX_LOG_LINES) buf.splice(0, buf.length - MAX_LOG_LINES);
          return { logs: { ...s.logs, [projectId]: buf } };
        });

        if (ev.payload.kind === "run" && ev.payload.runId) {
          const address = parseDevelopmentAddress(line);
          if (address) void trackAddress(projectId, ev.payload.runId, address);
        }
      });

      await listen<{ projectId: string; code: number | null; kind: "run" | "install"; runId: string }>(
        "process-exit",
        (ev) => {
          const { projectId, code, runId } = ev.payload;

          // 依赖安装进程退出（FR-11）：更新 installs 状态；
          // 成功且带 thenStart → 接着启动项目；失败 → toast + 自动弹日志抽屉
          if (ev.payload.kind === "install") {
            const st = get().installs[projectId];
            if (st?.runId === runId && st.status === "running") {
              const manual = manualInstallStops.delete(runId);
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

          if (get().runtimes[projectId]?.runId !== runId) return;
          const intentional = manualStops.has(runId);
          clearCachedAddress(runId);
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

      // 发布流水线步骤进度（FR-20）
      await listen<{
        projectId: string;
        step: ReleaseStep;
        status: ReleaseStepStatus;
        message?: string;
      }>("release-step", (ev) => {
        const { projectId, step: stepName, status, message } = ev.payload;
        set((s) => {
          const run: ReleaseRun =
            s.releaseRuns[projectId] ?? { running: true, steps: {}, messages: {} };
          return {
            releaseRuns: {
              ...s.releaseRuns,
              [projectId]: {
                ...run,
                running: true,
                steps: { ...run.steps, [stepName]: status },
                messages: { ...run.messages, [stepName]: message },
              },
            },
          };
        });
      });

      // 发布流水线结束（FR-20）：成功 toast + 打开入口；失败 toast + 自动弹日志抽屉
      await listen<{
        projectId: string;
        success: boolean;
        zipPath?: string;
        error?: string;
      }>("release-finished", (ev) => {
        const { projectId, success, zipPath, error } = ev.payload;
        set((s) => {
          const run: ReleaseRun =
            s.releaseRuns[projectId] ?? { running: false, steps: {}, messages: {} };
          return {
            releaseRuns: {
              ...s.releaseRuns,
              [projectId]: { ...run, running: false, zipPath, error },
            },
          };
        });
        const project = useProjectsStore
          .getState()
          .projects.find((p) => p.id === projectId);
        if (project) {
          if (success) {
            toast.success(`「${project.name}」发布完成，产物：${zipPath}`);
          } else {
            toast.error(`「${project.name}」发布失败：${error}`);
            set({ pendingErrorProjectId: projectId });
          }
        }
      });
    })();
    return listenersReady;
  },

  syncFromBackend: async () => {
    const initial = get().runtimes;
    const initialInstalls = get().installs;
    try {
      const [{ runs, installs: activeInstalls }, activeRelease] = await Promise.all([
        invoke<RunningProcesses>("get_running"),
        invoke<string | null>("get_active_release"),
      ]);
      const current = get();
      const runtimes = { ...current.runtimes };
      const installs = { ...current.installs };
      for (const [id, run] of Object.entries(runs)) {
        if (runtimes[id] === initial[id] && runtimes[id]?.runId !== run.runId) {
          runtimes[id] = { ...run, status: "running" };
        }
      }
      for (const [id, run] of Object.entries(runtimes)) {
        if (run === initial[id] && run.pid && isActive(run.status) && !runs[id]) {
          runtimes[id] = { status: "stopped" };
        }
      }
      for (const [id, install] of Object.entries(activeInstalls)) {
        if (installs[id] === initialInstalls[id] && installs[id]?.runId !== install.runId) {
          installs[id] = { runId: install.runId, status: "running", thenStart: false };
        }
      }
      for (const [id, install] of Object.entries(installs)) {
        if (install === initialInstalls[id] && install.status === "running" && !activeInstalls[id] && !startingInstalls.has(id)) {
          installs[id] = { status: "stopped", thenStart: false };
        }
      }
      const releaseRuns = { ...current.releaseRuns };
      if (activeRelease && !releaseRuns[activeRelease]) {
        releaseRuns[activeRelease] = { running: true, steps: {}, messages: {} };
      }
      set({ runtimes, installs, releaseRuns });
      for (const [id, run] of Object.entries(runs)) {
        const manualPort = useProjectsStore.getState().projects.find((project) => project.id === id)?.manualPort;
        const fallback = manualPort ? portAddress(manualPort, 0) : null;
        const address = readCachedAddress(run.runId) ?? fallback;
        if (address) void trackAddress(id, run.runId, address);
      }
    } catch (error) {
      console.error("同步项目进程失败", error);
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

  start: (project) => {
    const { id } = project;
    if (isActive(get().runtimes[id]?.status) || startingProjects.has(id)) return Promise.resolve("alreadyRunning");
    if (stoppingProjects.has(id) || get().removingProjects.has(id)) return Promise.resolve("failed");
    const runId = crypto.randomUUID();
    const startedAt = Date.now();
    setRuntime(id, { status: "starting", runId, startedAt });
    const cancelled = () => get().removingProjects.has(id) || stoppingProjects.has(id);
    const request = (async (): Promise<StartResult> => {
      try {
        const installed = await invoke<boolean>("check_deps", { path: project.path });
        if (cancelled()) return "failed";
        if (!installed) {
          setRuntime(id, { status: "stopped" });
          set((state) => ({ pendingInstall: [...state.pendingInstall.filter((item) => item.id !== id), project] }));
          return "needsDependencies";
        }
        const manual = project.manualPort ? portAddress(project.manualPort, 0) : null;
        if (manual && await invoke<boolean>("probe_port", { port: manual.port }).catch(() => false)) {
          toast.warning("端口 " + manual.port + " 已被占用，请留意「" + project.name + "」的实际运行地址");
        }
        if (cancelled()) return "failed";
        const pid = await invoke<number>("start_project", { project, runId });
        const current = get().runtimes[id];
        if (current?.runId !== runId) return "failed";
        setRuntime(id, { ...current, pid, status: current.status === "ready" ? "ready" : "running" });
        if (manual) void trackAddress(id, runId, manual);
        return "started";
      } catch (error) {
        if (get().runtimes[id]?.runId === runId) setRuntime(id, { status: "error" });
        toast.error("启动「" + project.name + "」失败: " + error);
        void get().syncFromBackend();
        return "failed";
      }
    })().finally(() => {
      startingProjects.delete(id);
      const current = get().runtimes[id];
      if (current?.status === "starting" && current.runId === runId) setRuntime(id, { status: "stopped" });
    });
    startingProjects.set(id, request);
    return request;
  },

  stop: (project) => {
    const { id } = project;
    const pending = stoppingProjects.get(id);
    if (pending) return pending;
    set((state) => {
      const installation = state.installs[id];
      return {
        pendingInstall: state.pendingInstall.filter((item) => item.id !== id),
        installs: installation?.thenStart
          ? { ...state.installs, [id]: { ...installation, thenStart: false } }
          : state.installs,
      };
    });
    const request = (async () => {
      await startingProjects.get(id);
      const runId = get().runtimes[id]?.runId;
      if (runId) manualStops.add(runId);
      try {
        await invoke("stop_project", { projectId: id });
        if (get().runtimes[id]?.runId === runId) setRuntime(id, { status: "stopped" });
        if (runId) clearCachedAddress(runId);
        return true;
      } catch (error) {
        toast.error("停止「" + project.name + "」失败: " + error);
        return false;
      } finally {
        if (runId) manualStops.delete(runId);
      }
    })().finally(() => stoppingProjects.delete(id));
    stoppingProjects.set(id, request);
    return request;
  },

  restart: async (project) => {
    return await get().stop(project) ? get().start(project) : "failed";
  },

  install: (project, opts) => {
    const { id } = project;
    if (get().removingProjects.has(id) || stoppingInstalls.has(id) || get().installs[id]?.status === "running") return Promise.resolve();
    const runId = crypto.randomUUID();
    set((s) => ({
      installs: {
        ...s.installs,
        [id]: { runId, status: "running", thenStart: opts?.thenStart ?? false },
      },
    }));
    const request = invoke<number>("install_project", { project, runId }).then(() => {}).catch((error) => {
      if (get().installs[id]?.runId === runId) {
        set((s) => ({ installs: { ...s.installs, [id]: { status: "error", thenStart: false } } }));
      }
      toast.error("安装「" + project.name + "」依赖失败: " + error);
    }).finally(() => startingInstalls.delete(id));
    startingInstalls.set(id, request);
    return request;
  },

  resolvePendingInstall: (confirmed) => {
    const pending = get().pendingInstall;
    set({ pendingInstall: [] });
    if (confirmed) {
      for (const p of pending) void get().install(p, { thenStart: true });
    }
  },

  stopInstall: (project) => {
    const { id } = project;
    const pending = stoppingInstalls.get(id);
    if (pending) return pending;
    const request = (async () => {
      const runId = get().installs[id]?.runId;
      if (runId) manualInstallStops.add(runId);
      await startingInstalls.get(id);
      try {
        await invoke("stop_install", { projectId: id });
        if (get().installs[id]?.runId === runId) {
          set((s) => ({ installs: { ...s.installs, [id]: { status: "stopped", thenStart: false } } }));
        }
        return true;
      } catch (error) {
        toast.error("终止「" + project.name + "」依赖安装失败: " + error);
        return false;
      } finally {
        if (runId) manualInstallStops.delete(runId);
      }
    })().finally(() => stoppingInstalls.delete(id));
    stoppingInstalls.set(id, request);
    return request;
  },

  removeProjects: async (projects) => {
    const candidates = projects.filter(({ id }) => !get().removingProjects.has(id));
    if (candidates.length === 0) return { removed: [], failed: projects };
    const ids = new Set(candidates.map(({ id }) => id));
    set((state) => ({ removingProjects: new Set([...state.removingProjects, ...ids]) }));
    try {
      const activeRelease = await invoke<string | null>("get_active_release");
      const results = await Promise.all(candidates.map(async (project) => {
        if (get().releaseRuns[project.id]?.running || activeRelease === project.id) return false;
        const [stopped, installStopped] = await Promise.all([get().stop(project), get().stopInstall(project)]);
        return stopped && installStopped;
      }));
      const removed = candidates.filter((_, index) => results[index]);
      await useProjectsStore.getState().removeMany(removed.map(({ id }) => id));
      const removedIds = new Set(removed.map(({ id }) => id));
      set((state) => ({
        runtimes: Object.fromEntries(Object.entries(state.runtimes).filter(([id]) => !removedIds.has(id))),
        installs: Object.fromEntries(Object.entries(state.installs).filter(([id]) => !removedIds.has(id))),
        logs: Object.fromEntries(Object.entries(state.logs).filter(([id]) => !removedIds.has(id))),
        releaseRuns: Object.fromEntries(Object.entries(state.releaseRuns).filter(([id]) => !removedIds.has(id))),
      }));
      return { removed, failed: projects.filter(({ id }) => !removedIds.has(id)) };
    } finally {
      set((state) => ({ removingProjects: new Set([...state.removingProjects].filter((id) => !ids.has(id))) }));
    }
  },

  startRelease: async (project, newVersion, expectedVersion, announcement) => {
    if (!project.release) {
      toast.error("该项目未配置发布参数，请先在编辑里填写");
      return;
    }
    if (get().removingProjects.has(project.id) || get().releaseRuns[project.id]?.running) return;
    set((s) => ({
      releaseRuns: {
        ...s.releaseRuns,
        [project.id]: { running: true, steps: {}, messages: {} },
      },
    }));
    try {
      await invoke("start_release", { project, newVersion, expectedVersion, announcement });
    } catch (e) {
      set((s) => ({
        releaseRuns: {
          ...s.releaseRuns,
          [project.id]: { running: false, steps: {}, messages: {}, error: String(e) },
        },
      }));
      toast.error(`启动发布失败: ${e}`);
    }
  },
}));
