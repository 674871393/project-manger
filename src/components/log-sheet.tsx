import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { ArrowDown, Copy, Trash } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import type { Project } from "@/types";
import { useRuntimeStore } from "@/stores/runtime";

interface Props {
  project: Project | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/** 日志抽屉（PRD FR-4）：实时滚动、stderr 着色、复制、清空、暂停跟随 */
export function LogSheet({ project, open, onOpenChange }: Props) {
  const logs = useRuntimeStore((s) => s.logs);
  const status = useRuntimeStore((s) => project ? s.runtimes[project.id]?.status : undefined);
  const clearLogs = useRuntimeStore((s) => s.clearLogs);

  const scrollRef = useRef<HTMLDivElement>(null);
  // 用户是否在底部附近；向上滚就暂停自动跟随
  const [follow, setFollow] = useState(true);
  const lines = project ? logs[project.id] ?? [] : [];

  useEffect(() => {
    if (!follow || !open) return;
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lines.length, follow, open]);

  // 每次打开重置为跟随模式
  useEffect(() => {
    if (open) setFollow(true);
  }, [open]);

  function handleScroll() {
    const el = scrollRef.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    setFollow(atBottom);
  }

  async function handleCopy() {
    if (!project || lines.length === 0) return;
    const text = lines.map((l) => l.line).join("\n");
    await navigator.clipboard.writeText(text);
    toast.success(`已复制 ${lines.length} 行日志`);
  }

  function handleClear() {
    if (project) clearLogs(project.id);
  }

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
          </SheetTitle>
          <SheetDescription>
            stdout + stderr 合并输出，共 {lines.length} 行
            {status !== "running" && "（项目未运行，以下为历史日志）"}
          </SheetDescription>
        </SheetHeader>

        <div
          ref={scrollRef}
          onScroll={handleScroll}
          className="flex-1 overflow-y-auto bg-muted/40 p-3 font-mono text-xs leading-5"
        >
          {lines.length === 0 ? (
            <div className="flex h-full items-center justify-center text-muted-foreground">
              {status === "running" ? "等待输出…" : "暂无日志"}
            </div>
          ) : (
            <div className="space-y-0.5 whitespace-pre-wrap break-all">
              {lines.map((l, i) => (
                <div
                  key={i}
                  className={
                    l.stream === "stderr"
                      ? "text-red-600 dark:text-red-400"
                      : "text-foreground"
                  }
                >
                  {l.line}
                </div>
              ))}
            </div>
          )}
        </div>

        <div className="flex items-center justify-between border-t p-2">
          <span className="px-2 text-xs text-muted-foreground">
            {follow ? "跟随最新输出" : "已暂停跟随"}
          </span>
          <div className="flex gap-1">
            {!follow && (
              <Button variant="outline" size="sm" onClick={() => setFollow(true)}>
                <ArrowDown />
                回到最新
              </Button>
            )}
            <Button variant="outline" size="sm" onClick={handleCopy}>
              <Copy />
              复制
            </Button>
            <Button variant="outline" size="sm" onClick={handleClear}>
              <Trash />
              清空
            </Button>
          </div>
        </div>
      </SheetContent>
    </Sheet>
  );
}
