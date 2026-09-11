import { useEffect, useMemo, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { FolderSearch, Plus, RefreshCw, Search, Settings, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { TooltipProvider } from "@/components/ui/tooltip";
import { Toaster } from "@/components/ui/sonner";
import { ProjectFormDialog } from "@/components/project-form-dialog";
import { ProjectTable } from "@/components/project-table";
import { LogSheet } from "@/components/log-sheet";
import { ScanImportDialog } from "@/components/scan-import-dialog";
import { SettingsDialog } from "@/components/settings-dialog";
import { InstallConfirmDialog } from "@/components/install-confirm-dialog";
import { ReleaseDialog } from "@/components/release-dialog";
import type { Project } from "@/types";
import { useProjectsStore } from "@/stores/projects";
import { useRuntimeStore } from "@/stores/runtime";

function App() {
  const { projects, branches, loaded, load, refreshBranches } =
    useProjectsStore();
  const initListeners = useRuntimeStore((s) => s.initListeners);
  const loadLanIp = useRuntimeStore((s) => s.loadLanIp);
  const syncFromBackend = useRuntimeStore((s) => s.syncFromBackend);
  const pendingErrorProjectId = useRuntimeStore((s) => s.pendingErrorProjectId);

  const [search, setSearch] = useState("");
  const [formOpen, setFormOpen] = useState(false);
  const [editing, setEditing] = useState<Project | null>(null);
  const [logProject, setLogProject] = useState<Project | null>(null);
  const [logOpen, setLogOpen] = useState(false);
  const [scanOpen, setScanOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [releaseProject, setReleaseProject] = useState<Project | null>(null);
  const [releaseOpen, setReleaseOpen] = useState(false);

  useEffect(() => {
    load();
    initListeners();
    loadLanIp();
    syncFromBackend();
  }, [load, initListeners, loadLanIp, syncFromBackend]);

  // 进程意外退出 → 自动弹出该项目的日志抽屉（PRD 4.3）
  useEffect(() => {
    if (!pendingErrorProjectId) return;
    const project = useProjectsStore
      .getState()
      .projects.find((p) => p.id === pendingErrorProjectId);
    if (project) {
      setLogProject(project);
      setLogOpen(true);
    }
    useRuntimeStore.setState({ pendingErrorProjectId: null });
  }, [pendingErrorProjectId]);

  // 分支定时轮询（PRD FR-6 P2：30s）
  useEffect(() => {
    const timer = setInterval(() => {
      useProjectsStore.getState().refreshBranches();
    }, 30_000);
    return () => clearInterval(timer);
  }, []);

  // 窗口失焦淡化：焦点事件驱动 body.blur-dim（styles.css）
  useEffect(() => {
    const win = getCurrentWindow();
    let unlisten: (() => void) | undefined;
    win
      .onFocusChanged(({ payload: focused }) =>
        document.body.classList.toggle("blur-dim", !focused),
      )
      .then((u) => (unlisten = u))
      .catch(() => {});
    return () => {
      unlisten?.();
      document.body.classList.remove("blur-dim");
    };
  }, []);

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    if (!q) return projects;
    return projects.filter(
      (p) =>
        p.name.toLowerCase().includes(q) ||
        p.path.toLowerCase().includes(q),
    );
  }, [projects, search]);

  function openAdd() {
    setEditing(null);
    setFormOpen(true);
  }

  function openEdit(project: Project) {
    setEditing(project);
    setFormOpen(true);
  }

  return (
    <TooltipProvider>
      <div className="flex h-screen flex-col">
        <header className="flex items-center justify-between gap-4 border-b px-4 py-2">
          <h1 className="text-sm font-semibold">前端项目管理器</h1>
          <div className="flex items-center gap-2">
            <div className="relative">
              <Search className="absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={search}
                onChange={(e) => setSearch(e.currentTarget.value)}
                placeholder="搜索项目…"
                className="w-56 pr-8 pl-8"
              />
              {search && (
                <button
                  type="button"
                  aria-label="清空搜索"
                  className="absolute top-1/2 right-2 -translate-y-1/2 cursor-pointer text-muted-foreground hover:text-foreground"
                  onClick={() => setSearch("")}
                >
                  <X className="size-4" />
                </button>
              )}
            </div>
            <Button
              variant="outline"
              size="sm"
              onClick={refreshBranches}
              title="重新读取项目列表与分支"
            >
              <RefreshCw />
              刷新
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setScanOpen(true)}
              title="扫描目录批量导入"
            >
              <FolderSearch />
              扫描导入
            </Button>
            <Button size="sm" onClick={openAdd}>
              <Plus />
              添加项目
            </Button>
            <Button
              variant="outline"
              size="icon-sm"
              onClick={() => setSettingsOpen(true)}
              title="设置"
            >
              <Settings />
            </Button>
          </div>
        </header>

        <main className="flex-1 overflow-auto p-4">
          {projects.length === 0 ? (
            <div className="flex h-full items-center justify-center rounded-lg border border-dashed text-muted-foreground">
              {loaded
                ? "暂无项目 — 点击右上角「添加项目」开始"
                : "正在加载…"}
            </div>
          ) : filtered.length === 0 ? (
            <div className="flex h-full items-center justify-center rounded-lg border border-dashed text-muted-foreground">
              没有匹配「{search}」的项目
            </div>
          ) : (
            <ProjectTable
              projects={filtered}
              branches={branches}
              onEdit={openEdit}
              onOpenLogs={(p) => {
                setLogProject(p);
                setLogOpen(true);
              }}
              onRelease={(p) => {
                setReleaseProject(p);
                setReleaseOpen(true);
              }}
            />
          )}
        </main>
      </div>

      <ProjectFormDialog
        initial={editing}
        open={formOpen}
        onOpenChange={setFormOpen}
      />
      <ScanImportDialog open={scanOpen} onOpenChange={setScanOpen} />
      <SettingsDialog open={settingsOpen} onOpenChange={setSettingsOpen} />
      <InstallConfirmDialog />
      <ReleaseDialog
        project={releaseProject}
        open={releaseOpen}
        onOpenChange={setReleaseOpen}
      />
      <LogSheet
        project={logProject}
        open={logOpen}
        onOpenChange={setLogOpen}
      />
      <Toaster />
    </TooltipProvider>
  );
}

export default App;
