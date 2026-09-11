import { useEffect, useRef, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "sonner";
import {
  ArrowDown,
  Copy,
  History,
  Loader2,
  Search,
  Square,
  Trash,
  X,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import type { Project } from "@/types";
import { useRuntimeStore, type LogLine } from "@/stores/runtime";

interface Props {
  project: Project | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/** 行高 = text-xs + leading-5 → 20px；虚拟滚动按固定行高开窗（PRD FR-10 虚拟滚动） */
const ROW_HEIGHT = 20;
/** 视口外多渲染的行数：快速滚动不露白 */
const OVERSCAN = 20;
/** 未运行时从落盘文件读回的历史日志行数 */
const HISTORY_LINES = 500;

/** 按关键字高亮渲染一行（大小写不敏感），无关键字原样返回 */
function HighlightedLine({ line, query }: { line: string; query: string }) {
  const q = query.trim();
  if (!q) return <>{line}</>;
  const lower = line.toLowerCase();
  const target = q.toLowerCase();
  const parts: ReactNode[] = [];
  let from = 0;
  while (from <= line.length - target.length) {
    const idx = lower.indexOf(target, from);
    if (idx === -1) break;
    if (idx > from) parts.push(line.slice(from, idx));
    parts.push(
      <mark
        key={parts.length}
        className="rounded-sm bg-yellow-200/80 text-inherit dark:bg-yellow-500/30"
      >
        {line.slice(idx, idx + q.length)}
      </mark>,
    );
    from = idx + q.length;
  }
  parts.push(line.slice(from));
  return <>{parts}</>;
}

/** 日志抽屉（PRD FR-4 / FR-10）：实时滚动、stderr 着色、复制、清空、暂停跟随、
 * 关键字过滤高亮、虚拟滚动、未运行时展示上次会话尾部日志 */
export function LogSheet({ project, open, onOpenChange }: Props) {
  const logs = useRuntimeStore((s) => s.logs);
  const installs = useRuntimeStore((s) => s.installs);
  const status = useRuntimeStore((s) =>
    project ? s.runtimes[project.id]?.status : undefined,
  );
  const clearLogs = useRuntimeStore((s) => s.clearLogs);
  const stopInstall = useRuntimeStore((s) => s.stopInstall);

  // 用户是否在底部附近；向上滚就暂停自动跟随
  const [follow, setFollow] = useState(true);
  // 关键字过滤（FR-10）
  const [query, setQuery] = useState("");
  // 上次会话的落盘日志（项目未运行且本会话无内存日志时读回，FR-10）
  const [history, setHistory] = useState<LogLine[]>([]);
  // 虚拟滚动的视口状态
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportH, setViewportH] = useState(0);

  const scrollRef = useRef<HTMLDivElement>(null);

  const memLines = project ? logs[project.id] ?? [] : [];
  const installState = project ? installs[project.id] : undefined;
  // 本会话有内存日志优先展示；否则回退历史日志
  const allLines = memLines.length > 0 ? memLines : history;

  const q = query.trim().toLowerCase();
  const displayLines = q
    ? allLines.filter((l) => l.line.toLowerCase().includes(q))
    : allLines;

  // 切换项目 / 重开抽屉时重置过滤与历史
  useEffect(() => {
    if (open) {
      setQuery("");
      setHistory([]);
      setFollow(true);
    }
  }, [open, project?.id]);

  // 项目未运行且无内存日志 → 读回落盘的尾部历史（FR-10 跨会话日志）
  useEffect(() => {
    if (!open || !project) return;
    if (status === "running" || status === "starting") return;
    if ((useRuntimeStore.getState().logs[project.id] ?? []).length > 0) return;
    let cancelled = false;
    invoke<string[]>("read_log_tail", {
      projectId: project.id,
      maxLines: HISTORY_LINES,
    })
      .then((tail) => {
        if (cancelled) return;
        setHistory(
          tail.map((line) => ({
            stream: "stdout" as const,
            line,
            historical: true,
          })),
        );
      })
      .catch(() => {
        // 落盘日志不可用（首跑/被清理）：保持空，显示「暂无日志」
        if (!cancelled) setHistory([]);
      });
    return () => {
      cancelled = true;
    };
  }, [open, project, status]);

  // 跟随最新输出
  useEffect(() => {
    if (!follow || !open) return;
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [displayLines.length, follow, open]);

  // 视口高度变化（窗口缩放/抽屉开合）时重算虚拟滚动窗口
  useEffect(() => {
    const el = scrollRef.current;
    if (!el || !open) return;
    const ro = new ResizeObserver(() => setViewportH(el.clientHeight));
    ro.observe(el);
    setViewportH(el.clientHeight);
    return () => ro.disconnect();
  }, [open]);

  function handleScroll() {
    const el = scrollRef.current;
    if (!el) return;
    setScrollTop(el.scrollTop);
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    setFollow(atBottom);
  }

  // 虚拟滚动窗口：只渲染视口 ± OVERSCAN 范围内的行
  const startIdx = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN);
  const endIdx = Math.min(
    displayLines.length,
    Math.ceil((scrollTop + viewportH) / ROW_HEIGHT) + OVERSCAN,
  );

  async function handleCopy() {
    if (displayLines.length === 0) return;
    const text = displayLines.map((l) => l.line).join("\n");
    await navigator.clipboard.writeText(text);
    toast.success(
      query ? `已复制 ${displayLines.length} 行（已过滤）` : `已复制 ${displayLines.length} 行日志`,
    );
  }

  function handleClear() {
    if (project) clearLogs(project.id);
  }

  const showingHistory = memLines.length === 0 && history.length > 0;

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent side="right" className="w-full gap-0 sm:max-w-[50vw]">
        <SheetHeader className="border-b pr-12">
          <SheetTitle className="flex items-center gap-2">
            {project?.name}
            {status === "running" ? (
              <Badge className="border-transparent bg-emerald-500/15 text-emerald-700 dark:text-emerald-400">
                运行中
              </Badge>
            ) : status === "error" ? (
              <Badge variant="destructive">异常</Badge>
            ) : (
              <Badge variant="secondary">未运行</Badge>
            )}
            {installState?.status === "running" && (
              <Badge className="gap-1 border-transparent bg-blue-500/15 text-blue-700 dark:text-blue-400">
                <Loader2 className="size-3 animate-spin" />
                安装依赖中
              </Badge>
            )}
          </SheetTitle>
          <SheetDescription>
            {showingHistory ? (
              <span className="inline-flex items-center gap-1">
                <History className="size-3" />
                上次会话的尾部日志，共 {history.length} 行（项目未运行）
              </span>
            ) : (
              <>
                stdout + stderr 合并输出，共 {allLines.length} 行
                {status !== "running" && "（项目未运行）"}
              </>
            )}
          </SheetDescription>
        </SheetHeader>

        {/* 关键字过滤（FR-10）：大小写不敏感，命中行高亮 */}
        <div className="flex items-center gap-2 border-b px-3 py-2">
          <div className="relative flex-1">
            <Search className="absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={query}
              onChange={(e) => setQuery(e.currentTarget.value)}
              placeholder="过滤日志…"
              className="h-8 pr-7 pl-8 text-xs"
            />
            {query && (
              <button
                type="button"
                aria-label="清空过滤"
                className="absolute top-1/2 right-2 -translate-y-1/2 cursor-pointer text-muted-foreground hover:text-foreground"
                onClick={() => setQuery("")}
              >
                <X className="size-3.5" />
              </button>
            )}
          </div>
          {q && (
            <span className="text-xs whitespace-nowrap text-muted-foreground">
              {displayLines.length}/{allLines.length} 行
            </span>
          )}
        </div>

        {/* 日志主体：固定行高虚拟滚动；whitespace-pre 不换行，长行横向滚动 */}
        <div
          ref={scrollRef}
          onScroll={handleScroll}
          className="flex-1 overflow-auto bg-muted/40 font-mono text-xs leading-5"
        >
          {displayLines.length === 0 ? (
            <div className="flex h-full items-center justify-center text-muted-foreground">
              {q
                ? `没有匹配「${query.trim()}」的日志行`
                : status === "running" || status === "starting"
                  ? "等待输出…"
                  : "暂无日志"}
            </div>
          ) : (
            <div
              style={{ height: displayLines.length * ROW_HEIGHT }}
              className="relative"
            >
              {displayLines.slice(startIdx, endIdx).map((l, i) => {
                const idx = startIdx + i;
                return (
                  <div
                    key={idx}
                    style={{
                      position: "absolute",
                      top: idx * ROW_HEIGHT,
                      height: ROW_HEIGHT,
                      whiteSpace: "pre",
                    }}
                    className={
                      l.stream === "stderr"
                        ? "text-red-600 dark:text-red-400"
                        : l.historical
                          ? "text-muted-foreground"
                          : "text-foreground"
                    }
                  >
                    {l.tag === "install" && (
                      <span className="mr-1 text-muted-foreground">
                        [install]
                      </span>
                    )}
                    <HighlightedLine line={l.line} query={query} />
                  </div>
                );
              })}
            </div>
          )}
        </div>

        <div className="flex items-center justify-between border-t p-2">
          <span className="flex items-center gap-1.5 px-2 text-xs text-muted-foreground">
            {installState?.status === "running" ? (
              <>
                <Loader2 className="size-3 animate-spin" />
                正在安装依赖
              </>
            ) : (
              (follow ? "跟随最新输出" : "已暂停跟随")
            )}
          </span>
          <div className="flex gap-1">
            {installState?.status === "running" ? (
              <Button
                variant="outline"
                size="sm"
                onClick={() => project && stopInstall(project)}
              >
                <Square />
                终止安装
              </Button>
            ) : (
              <>
                {!follow && (
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setFollow(true)}
                  >
                    <ArrowDown />
                    回到最新
                  </Button>
                )}
                <Button variant="outline" size="sm" onClick={handleCopy}>
                  <Copy />
                  复制
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={handleClear}
                  disabled={memLines.length === 0}
                >
                  <Trash />
                  清空
                </Button>
              </>
            )}
          </div>
        </div>
      </SheetContent>
    </Sheet>
  );
}
