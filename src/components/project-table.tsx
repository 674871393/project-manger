import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "sonner";
import {
  Circle,
  CircleX,
  Copy,
  FolderOpen,
  GitBranch,
  Loader2,
  Pencil,
  Play,
  RotateCcw,
  ScrollText,
  Square,
  SquareCode,
  Trash2,
} from "lucide-react";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import type { Project, RunStatus } from "@/types";
import type { ProjectRuntime } from "@/stores/runtime";
import { useProjectsStore } from "@/stores/projects";
import { useRuntimeStore } from "@/stores/runtime";

interface Props {
  projects: Project[];
  /** projectId -> 分支名（null = 非 Git 目录） */
  branches: Record<string, string | null>;
  onEdit: (project: Project) => void;
  /** 打开日志抽屉 */
  onOpenLogs: (project: Project) => void;
}

function StatusBadge({ runtime }: { runtime?: ProjectRuntime }) {
  const status: RunStatus = runtime?.status ?? "stopped";

  if (status === "running") {
    return (
      <Badge className="gap-1.5 border-transparent bg-emerald-500/15 text-emerald-700 dark:text-emerald-400">
        <Circle className="size-2 fill-current" />
        运行中
      </Badge>
    );
  }
  if (status === "starting") {
    return (
      <Badge className="gap-1.5 border-transparent bg-yellow-500/15 text-yellow-700 dark:text-yellow-400">
        <Loader2 className="size-3 animate-spin" />
        启动中
      </Badge>
    );
  }
  if (status === "error") {
    return (
      <Tooltip>
        <TooltipTrigger asChild>
          <Badge variant="destructive" className="gap-1.5">
            <CircleX className="size-3" />
            异常
          </Badge>
        </TooltipTrigger>
        <TooltipContent>
          {runtime?.exitCode !== undefined
            ? `退出码 ${runtime.exitCode}`
            : "进程意外退出"}
        </TooltipContent>
      </Tooltip>
    );
  }
  return (
    <Badge variant="secondary" className="gap-1.5">
      <Circle className="size-2 fill-current" />
      已停止
    </Badge>
  );
}

/** 项目列表（PRD FR-2）：勾选 | 名称 | 路径 | 运行地址 | 状态 | 分支 | 操作 */
export function ProjectTable({ projects, branches, onEdit, onOpenLogs }: Props) {
  const remove = useProjectsStore((s) => s.remove);
  const runtimes = useRuntimeStore((s) => s.runtimes);
  const lanIp = useRuntimeStore((s) => s.lanIp);
  const start = useRuntimeStore((s) => s.start);
  const stop = useRuntimeStore((s) => s.stop);
  const restart = useRuntimeStore((s) => s.restart);

  const [selected, setSelected] = useState<Set<string>>(new Set());

  const toggleSelected = (id: string, checked: boolean) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (checked) next.add(id);
      else next.delete(id);
      return next;
    });
  };

  const allChecked =
    projects.length > 0 && projects.every((p) => selected.has(p.id));

  function toggleAll(checked: boolean) {
    setSelected(checked ? new Set(projects.map((p) => p.id)) : new Set());
  }

  const selectedProjects = projects.filter((p) => selected.has(p.id));

  async function handleBatchStart() {
    const targets = selectedProjects.filter(
      (p) => !["running", "starting"].includes(runtimes[p.id]?.status ?? "stopped"),
    );
    if (targets.length === 0) {
      toast.info("选中的项目都已在运行");
      return;
    }
    // 逐个 await，避免同时 spawn 多个 cmd 抢锁（start 内部锁是短临界区，串行即可）
    for (const p of targets) await start(p);
    toast.success(`已批量启动 ${targets.length} 个项目`);
  }

  async function handleBatchStop() {
    const targets = selectedProjects.filter(
      (p) => (runtimes[p.id]?.status ?? "stopped") === "running",
    );
    if (targets.length === 0) {
      toast.info("选中的项目都未在运行");
      return;
    }
    // 并行停止：每个项目独立进程树，互不依赖
    await Promise.all(targets.map((p) => stop(p)));
    toast.success(`已批量停止 ${targets.length} 个项目`);
  }

  async function handleBatchRestart() {
    const targets = selectedProjects.filter((p) => {
      const st = runtimes[p.id]?.status ?? "stopped";
      return st === "running" || st === "error";
    });
    if (targets.length === 0) {
      toast.info("选中的项目没有可重启的（未在运行且无异常）");
      return;
    }
    for (const p of targets) await restart(p);
    toast.success(`已批量重启 ${targets.length} 个项目`);
  }

  async function handleBatchVscode() {
    let ok = 0;
    for (const p of selectedProjects) {
      try {
        await invoke("open_in_vscode", { path: p.path });
        ok++;
      } catch {
        // 单个失败继续其余，最后统一报数
      }
    }
    if (ok === selectedProjects.length) {
      toast.success(`已在 VSCode 中打开 ${ok} 个项目`);
    } else {
      toast.warning(`${ok}/${selectedProjects.length} 个项目已打开，部分失败`);
    }
  }

  async function handleBatchDelete() {
    const running = selectedProjects.filter(
      (p) => (runtimes[p.id]?.status ?? "stopped") === "running",
    );
    try {
      // 先停止所有运行中的
      await Promise.all(running.map((p) => stop(p)));
      await Promise.all(selectedProjects.map((p) => remove(p.id)));
      setSelected(new Set());
      toast.success(`已移除 ${selectedProjects.length} 个项目（磁盘文件不受影响）`);
    } catch (e) {
      toast.error(`批量删除失败: ${e}`);
    }
  }

  async function handleDelete(project: Project, running: boolean) {
    try {
      if (running) await stop(project);
      await remove(project.id);
      // 同步清理勾选状态，避免工具栏残留已删项目的计数
      setSelected((prev) => {
        const next = new Set(prev);
        next.delete(project.id);
        return next;
      });
      toast.success(`已移除「${project.name}」（磁盘文件不受影响）`);
    } catch (e) {
      toast.error(`删除失败: ${e}`);
    }
  }

  return (
    <div className="space-y-2">
      {/* 批量操作工具栏：常显，未勾选时按钮置灰 */}
      <div className="flex items-center justify-between rounded-lg border bg-muted/50 px-3 py-1.5">
        <span className="text-sm text-muted-foreground">
          {selectedProjects.length > 0
            ? `已选 ${selectedProjects.length} 个项目`
            : "未勾选项目"}
        </span>
        <div className="flex gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={selectedProjects.length === 0}
            onClick={handleBatchStart}
          >
            <Play />
            批量启动
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={selectedProjects.length === 0}
            onClick={handleBatchStop}
          >
            <Square />
            批量停止
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={selectedProjects.length === 0}
            onClick={handleBatchRestart}
          >
            <RotateCcw />
            批量重启
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={selectedProjects.length === 0}
            onClick={handleBatchVscode}
          >
            <SquareCode className="text-foreground" />
            批量 VSCode
          </Button>
          <AlertDialog>
            <AlertDialogTrigger asChild>
              <Button
                variant="outline"
                size="sm"
                className="text-destructive"
                disabled={selectedProjects.length === 0}
              >
                <Trash2 />
                批量删除
              </Button>
            </AlertDialogTrigger>
              <AlertDialogContent size="sm">
                <AlertDialogHeader>
                  <AlertDialogTitle>
                    确认移除 {selectedProjects.length} 个项目？
                  </AlertDialogTitle>
                  <AlertDialogDescription>
                    仅从列表移除记录，不删除磁盘上的项目文件。
                    {selectedProjects.some(
                      (p) => (runtimes[p.id]?.status ?? "stopped") === "running",
                    ) && " 部分项目正在运行，将先停止其进程。"}
                  </AlertDialogDescription>
                </AlertDialogHeader>
                <AlertDialogFooter>
                  <AlertDialogCancel>取消</AlertDialogCancel>
                  <AlertDialogAction onClick={handleBatchDelete}>
                    移除
                  </AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
          </div>
      </div>

      <div className="rounded-lg border bg-card">
        <Table>
          <TableHeader>
            <TableRow className="hover:bg-transparent">
              <TableHead className="w-10 pl-4">
                <Checkbox
                  checked={allChecked}
                  onCheckedChange={(v) => toggleAll(v === true)}
                  aria-label="全选"
                />
              </TableHead>
              <TableHead>项目名称</TableHead>
              <TableHead>项目路径</TableHead>
              <TableHead>运行地址</TableHead>
              <TableHead>状态</TableHead>
              <TableHead>当前分支</TableHead>
              <TableHead className="pr-4">操作</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {projects.map((project) => {
              const branch = branches[project.id];
              const runtime = runtimes[project.id];
              const status: RunStatus = runtime?.status ?? "stopped";
              const canStart = status === "stopped" || status === "error";
              // 展示用局域网地址（手机同 WiFi 可访问）；点击打开仍走 localhost 最稳
              const displayUrl = lanIp && runtime?.url
                ? runtime.url.replace(/\/\/(localhost|127\.0\.0\.1)/, `//${lanIp}`)
                : runtime?.url;

              return (
                <TableRow key={project.id}>
                  <TableCell className="pl-4">
                    <Checkbox
                      checked={selected.has(project.id)}
                      onCheckedChange={(v) =>
                        toggleSelected(project.id, v === true)
                      }
                      aria-label={`选中 ${project.name}`}
                    />
                  </TableCell>

                  <TableCell className="relative overflow-hidden">
                    {/* 框架标识：斜置背景水印，纯装饰不可选中/复制（aria-hidden + select-none） */}
                    {project.framework && project.framework !== "other" && (
                      <span
                        aria-hidden="true"
                        className="pointer-events-none absolute top-1/2 right-3 -translate-y-1/2 -rotate-12 font-black tracking-[0.2em] text-lg text-foreground/10 uppercase select-none"
                      >
                        {project.framework}
                      </span>
                    )}
                    <div className="relative flex items-center gap-2 font-medium">
                      <span className="cursor-default">{project.name}</span>
                    </div>
                  </TableCell>

                  <TableCell>
                    <Tooltip>
                      <TooltipTrigger asChild>
                        <button
                          type="button"
                          className="inline-flex max-w-72 cursor-pointer items-center gap-1 truncate text-sm text-muted-foreground hover:text-foreground"
                          onClick={() => {
                            revealItemInDir(project.path).catch((e) => {
                              toast.error(`打开资源管理器失败: ${e}`);
                            });
                          }}
                          title="点击在资源管理器中打开"
                        >
                          <FolderOpen className="size-3.5 shrink-0" />
                          <span className="truncate">{project.path}</span>
                        </button>
                      </TooltipTrigger>
                      <TooltipContent>{project.path}</TooltipContent>
                    </Tooltip>
                  </TableCell>

                  <TableCell className="text-center">
                    {runtime?.url && status === "running" ? (
                      <div className="inline-flex items-center gap-1">
                        <button
                          type="button"
                          className="cursor-pointer text-sm text-primary underline-offset-4 hover:underline"
                          onClick={() => openUrl(runtime.url!)}
                          title="用默认浏览器打开"
                        >
                          {displayUrl}
                        </button>
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Button
                              variant="ghost"
                              size="icon-sm"
                              aria-label={`复制 ${project.name} 运行地址`}
                              onClick={async () => {
                                await navigator.clipboard.writeText(displayUrl ?? runtime.url!);
                                toast.success("地址已复制");
                              }}
                            >
                              <Copy />
                            </Button>
                          </TooltipTrigger>
                          <TooltipContent>复制地址</TooltipContent>
                        </Tooltip>
                      </div>
                    ) : project.manualPort ? (
                      <span className="text-sm text-muted-foreground">
                        http://localhost:{project.manualPort}
                      </span>
                    ) : (
                      <span className="text-muted-foreground">—</span>
                    )}
                  </TableCell>

                  <TableCell className="text-center">
                    <StatusBadge runtime={runtime} />
                  </TableCell>

                  <TableCell className="text-center">
                    {branch ? (
                      <span className="inline-flex items-center gap-1 text-sm">
                        <GitBranch className="size-3.5 text-muted-foreground" />
                        {branch}
                      </span>
                    ) : (
                      <span className="text-muted-foreground">-</span>
                    )}
                  </TableCell>

                  <TableCell className="pr-4">
                    <div className="flex justify-center gap-0.5">
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <Button
                            variant="ghost"
                            size="icon"
                            onClick={() => onOpenLogs(project)}
                            aria-label={`查看 ${project.name} 日志`}
                          >
                            <ScrollText />
                          </Button>
                        </TooltipTrigger>
                        <TooltipContent>日志</TooltipContent>
                      </Tooltip>

                      <Tooltip>
                        <TooltipTrigger asChild>
                          <Button
                            variant="ghost"
                            size="icon"
                            aria-label={`在 VSCode 中打开 ${project.name}`}
                            onClick={() => {
                              invoke("open_in_vscode", { path: project.path }).catch(
                                (e) => toast.error(String(e)),
                              );
                            }}
                          >
                            <SquareCode className="text-foreground" />
                          </Button>
                        </TooltipTrigger>
                        <TooltipContent>在 VSCode 中打开</TooltipContent>
                      </Tooltip>

                      {status === "running" ? (
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Button
                              variant="ghost"
                              size="icon"
                              onClick={() => stop(project)}
                              aria-label={`停止 ${project.name}`}
                            >
                              <Square />
                            </Button>
                          </TooltipTrigger>
                          <TooltipContent>停止</TooltipContent>
                        </Tooltip>
                      ) : (
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Button
                              variant="ghost"
                              size="icon"
                              disabled={!canStart}
                              onClick={() => start(project)}
                              aria-label={`启动 ${project.name}`}
                            >
                              <Play />
                            </Button>
                          </TooltipTrigger>
                          <TooltipContent>启动</TooltipContent>
                        </Tooltip>
                      )}

                      <Tooltip>
                        <TooltipTrigger asChild>
                          <Button
                            variant="ghost"
                            size="icon"
                            disabled={status !== "running"}
                            onClick={() => restart(project)}
                            aria-label={`重启 ${project.name}`}
                          >
                            <RotateCcw />
                          </Button>
                        </TooltipTrigger>
                        <TooltipContent>重启</TooltipContent>
                      </Tooltip>

                      <Tooltip>
                        <TooltipTrigger asChild>
                          <Button
                            variant="ghost"
                            size="icon"
                            onClick={() => onEdit(project)}
                            aria-label={`编辑 ${project.name}`}
                          >
                            <Pencil />
                          </Button>
                        </TooltipTrigger>
                        <TooltipContent>编辑</TooltipContent>
                      </Tooltip>

                      <AlertDialog>
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <AlertDialogTrigger asChild>
                              <Button
                                variant="ghost"
                                size="icon"
                                aria-label={`删除 ${project.name}`}
                              >
                                <Trash2 className="text-destructive" />
                              </Button>
                            </AlertDialogTrigger>
                          </TooltipTrigger>
                          <TooltipContent>删除</TooltipContent>
                        </Tooltip>
                        <AlertDialogContent size="sm">
                          <AlertDialogHeader>
                            <AlertDialogTitle>
                              确认移除「{project.name}」？
                            </AlertDialogTitle>
                            <AlertDialogDescription>
                              仅从列表移除记录，不删除磁盘上的项目文件。
                              {status === "running" && " 项目正在运行，将先停止其进程。"}
                            </AlertDialogDescription>
                          </AlertDialogHeader>
                          <AlertDialogFooter>
                            <AlertDialogCancel>取消</AlertDialogCancel>
                            <AlertDialogAction
                              onClick={() => handleDelete(project, status === "running")}
                            >
                              移除
                            </AlertDialogAction>
                          </AlertDialogFooter>
                        </AlertDialogContent>
                      </AlertDialog>
                    </div>
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      </div>
    </div>
  );
}
