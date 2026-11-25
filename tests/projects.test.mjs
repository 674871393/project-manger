import assert from "node:assert/strict";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";
import { setImmediate as nextTurn } from "node:timers/promises";
import { createServer } from "vite";

const vite = await createServer({
  configFile: false,
  resolve: { alias: { "@": fileURLToPath(new URL("../src", import.meta.url)) } },
  server: { middlewareMode: true, ws: false, watch: null },
});
after(() => vite.close());

const handlers = new Map();
const storage = new Map();
const project = (id) => ({
  id, name: id, path: "D:/fixtures/" + id, packageManager: "npm", script: "dev", createdAt: 0,
});
let diskProjects = [project("first"), project("blocked"), project("last")];
let writes = 0;
let activeRuns = {};
let probe = async () => true;

globalThis.window = {
  __TAURI_INTERNALS__: {
    transformCallback: (handler) => handler,
    async invoke(command, payload) {
      switch (command) {
        case "plugin:event|listen":
          handlers.set(payload.event, payload.handler);
          return handlers.size;
        case "load_projects": return structuredClone(diskProjects);
        case "save_projects":
          await nextTurn();
          diskProjects = structuredClone(payload.projects);
          writes++;
          return;
        case "read_git_branch": return "main";
        case "get_active_release": return null;
        case "get_running": return { runs: structuredClone(activeRuns), installs: {} };
        case "check_deps": return true;
        case "start_project":
          activeRuns[payload.project.id] = { runId: payload.runId, pid: 10 };
          return 10;
        case "stop_project":
          if (payload.projectId === "blocked") throw new Error("进程没有停止");
          delete activeRuns[payload.projectId];
          return;
        case "stop_install": return;
        case "probe_project_port": return probe(payload);
        default: throw new Error("未实现测试命令 " + command);
      }
    },
  },
};
globalThis.sessionStorage = {
  getItem: (key) => storage.get(key) ?? null,
  setItem: (key, value) => storage.set(key, value),
  removeItem: (key) => storage.delete(key),
};

const { useProjectsStore } = await vite.ssrLoadModule("/src/stores/projects.ts");
const { useRuntimeStore } = await vite.ssrLoadModule("/src/stores/runtime.ts");
const projects = useProjectsStore.getState();
const runtime = useRuntimeStore.getState();
const emit = (event, payload) => handlers.get(event)({ event, payload });
await runtime.initListeners();

test("批量删除只写一次，重载后仅保留停止失败的项目", async () => {
  await projects.load();
  const report = await runtime.removeProjects(useProjectsStore.getState().projects);
  assert.deepEqual(report.removed.map(({ id }) => id), ["first", "last"]);
  assert.deepEqual(report.failed.map(({ id }) => id), ["blocked"]);
  assert.equal(writes, 1);
  await projects.load();
  assert.deepEqual(useProjectsStore.getState().projects.map(({ id }) => id), ["blocked"]);
});

test("保存项目列表时保留重叠操作的结果", async () => {
  await Promise.all([
    projects.add(project("new")),
    projects.removeMany(["blocked"]),
    projects.add(project("other")),
  ]);
  await projects.load();
  assert.deepEqual(useProjectsStore.getState().projects.map(({ id }) => id), ["new", "other"]);
});

test("仅凭本次开发进程的日志确认服务，忽略退出后返回的探测", async () => {
  const target = project("new");
  assert.equal(await runtime.start(target), "started");
  const state = () => useRuntimeStore.getState().runtimes[target.id];
  assert.equal(state().status, "running");
  const firstRun = state().runId;
  let reply;
  let calls = 0;
  probe = () => {
    calls++;
    return new Promise((resolve) => { reply = resolve; });
  };
  const output = { projectId: target.id, runId: firstRun, stream: "stdout", line: "Local: https://localhost:5173/admin/" };
  emit("log-line", { ...output, kind: "release" });
  emit("log-line", { ...output, kind: "install" });
  emit("log-line", { ...output, kind: "run", runId: "old" });
  assert.equal(calls, 0);
  emit("log-line", { ...output, kind: "run" });
  assert.equal(calls, 1);
  emit("process-exit", { projectId: target.id, kind: "run", runId: firstRun, code: 1 });
  reply(true);
  await nextTurn();
  assert.equal(state().status, "error");
  assert.equal(state().url, undefined);

  assert.equal(await runtime.start(target), "started");
  assert.notEqual(state().runId, firstRun);
  probe = async ({ port, host }) => {
    assert.equal(port, 5173);
    assert.equal(host, "localhost");
    return true;
  };
  emit("log-line", { ...output, kind: "run", runId: state().runId });
  await nextTurn();
  assert.equal(state().status, "ready");
  assert.equal(state().url, "https://localhost:5173/admin/");
});

test("Vue DevTools 日志和旧缓存都不会成为项目入口", async () => {
  const target = project("vite-with-devtools");
  const runId = "vite-resumed-with-old-cache";
  activeRuns[target.id] = { runId, pid: 10 };
  storage.set("fpm:address:" + runId, JSON.stringify({
    url: "http://localhost:3333/__devtools__/", host: "localhost", port: 3333, priority: 2,
  }));
  let calls = 0;
  probe = async () => { calls++; return true; };
  await runtime.syncFromBackend();
  const state = () => useRuntimeStore.getState().runtimes[target.id];
  assert.equal(calls, 0);
  const writeLog = (line) => emit("log-line", { projectId: target.id, runId, stream: "stdout", kind: "run", line });
  writeLog("➜  Vue DevTools: Open http://localhost:3333/__devtools__/ as a separate window");
  writeLog("http://localhost:3333/__vue-devtools__/?open=1");
  assert.equal(calls, 0);
  writeLog("➜  Local:   http://localhost:3333/");
  await nextTurn();
  assert.equal(state().status, "ready");
  assert.equal(state().url, "http://localhost:3333/");
  assert.equal(calls, 1);
});

test("框架明确输出的 Local 地址优先于同端口的普通日志地址", async () => {
  const target = project("vite-base-path");
  assert.equal(await runtime.start(target), "started");
  const state = () => useRuntimeStore.getState().runtimes[target.id];
  const writeLog = (line) => emit("log-line", { projectId: target.id, runId: state().runId, stream: "stdout", kind: "run", line });
  probe = async () => true;
  writeLog("http://localhost:3333/");
  await nextTurn();
  assert.equal(state().url, "http://localhost:3333/");
  writeLog("➜  Local:   https://localhost:3333/admin/");
  await nextTurn();
  assert.equal(state().url, "https://localhost:3333/admin/");
});
