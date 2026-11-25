import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { ArrowLeft, FolderOpen, History, RefreshCw } from "lucide-react";
import { toast } from "sonner";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import { stripAnsi } from "@/lib/project-address";
import type { ReleaseRecord } from "@/types";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

const formatDate = new Intl.DateTimeFormat("zh-CN", { dateStyle: "short", timeStyle: "short" });

function duration(ms: number): string {
  const seconds = Math.round(ms / 1000);
  if (seconds < 1) return "< 1 秒";
  if (seconds < 60) return seconds + " 秒";
  return Math.floor(seconds / 60) + " 分 " + (seconds % 60) + " 秒";
}

function ResultBadge({ success }: { success: boolean }) {
  return success ? <Badge variant="secondary">成功</Badge> : <Badge variant="destructive">失败</Badge>;
}

export function ReleaseHistorySheet({ open, onOpenChange }: Props) {
  const [records, setRecords] = useState<ReleaseRecord[] | null>(null);
  const [shown, setShown] = useState<ReleaseRecord | null>(null);
  const [logs, setLogs] = useState<string[] | null>(null);
  const [logError, setLogError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [refresh, setRefresh] = useState(0);

  useEffect(() => {
    if (!open) return;
    let ignore = false;
    setError(null);
    setRecords(null);
    invoke<ReleaseRecord[]>("list_release_history").then((data) => {
      if (!ignore) setRecords(data);
    }).catch((error) => { if (!ignore) setError(String(error)); });
    return () => { ignore = true; };
  }, [open, refresh]);

  useEffect(() => {
    if (!open || !shown) return;
    let ignore = false;
    setLogs(null);
    setLogError(null);
    invoke<string[]>("read_release_log", { releaseId: shown.id }).then((data) => {
      if (!ignore) setLogs(data.map(stripAnsi));
    }).catch((error) => { if (!ignore) setLogError(String(error)); });
    return () => { ignore = true; };
  }, [open, shown]);

  function toggle(next: boolean) {
    if (!next) setShown(null);
    onOpenChange(next);
  }

  const search = query.trim().toLowerCase();
  const filtered = records?.filter((record) => [record.projectName, record.version, record.branch, record.commit]
    .some((value) => value?.toLowerCase().includes(search)));

  return (
    <Sheet open={open} onOpenChange={toggle}>
      <SheetContent className="gap-0" style={{ width: "min(100%, 36rem)", maxWidth: "none" }}>
        <SheetHeader className="border-b pr-12">
          <SheetTitle className="flex items-center gap-2"><History className="size-4" />发布历史</SheetTitle>
          <SheetDescription>最近 100 次本地发布及对应日志。</SheetDescription>
        </SheetHeader>
        {shown ? (
          <div className="flex min-h-0 flex-1 flex-col gap-3 p-4">
            <div className="flex flex-wrap items-center gap-2">
              <Button variant="ghost" size="sm" onClick={() => setShown(null)}><ArrowLeft />返回列表</Button>
              <ResultBadge success={shown.success} />
              {shown.zipPath && <Button variant="outline" size="sm" className="ml-auto" onClick={() => {
                if (shown.zipPath) void revealItemInDir(shown.zipPath).catch((error) => toast.error("定位产物失败: " + error));
              }}><FolderOpen />打开产物位置</Button>}
            </div>
            <div className="min-w-0 text-sm">
              <p className="break-words font-medium">{shown.projectName} · v{shown.version}</p>
              <p className="mt-1 break-all text-muted-foreground" title={shown.commit ?? undefined}>{shown.branch} · {shown.commit?.slice(0, 12) ?? "未记录提交"} · Node {shown.nodeVersion}</p>
              <p className="text-muted-foreground">{formatDate.format(shown.startedAt)} · {duration(shown.durationMs)}</p>
              {shown.zipPath && <p className="mt-1 break-all font-mono text-xs text-muted-foreground">{shown.zipPath}</p>}
              {shown.error && <p className="mt-1 break-words text-destructive">{shown.error}</p>}
            </div>
            <div className="min-h-0 flex-1 overflow-auto rounded-lg border bg-muted/30 p-3" aria-label="该次发布的日志">
              {logError ? <p role="alert" className="text-sm text-destructive">{logError}</p> : (
                <pre className="whitespace-pre-wrap break-all font-mono text-xs leading-relaxed">{logs ? logs.join("\n") || "本次发布没有日志。" : "正在读取日志…"}</pre>
              )}
            </div>
          </div>
        ) : (
          <>
            <div className="flex items-center gap-2 p-4 pb-2">
              <Input value={query} onChange={(event) => setQuery(event.currentTarget.value)} placeholder="搜索项目、版本、分支或提交" aria-label="搜索发布记录" />
              <Button variant="outline" size="icon" onClick={() => setRefresh((value) => value + 1)} aria-label="刷新发布历史"><RefreshCw /></Button>
            </div>
            <div className="min-h-0 flex-1 space-y-2 overflow-auto p-4 pt-2">
              {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
              {!records && !error && <p className="text-sm text-muted-foreground">正在读取发布历史…</p>}
              {filtered?.length === 0 && <p className="rounded-lg border border-dashed p-4 text-sm text-muted-foreground">{search ? "没有匹配的发布记录。" : "完成一次本地发布后，结果会出现在这里。"}</p>}
              {filtered?.map((record) => (
                <button key={record.id} type="button" onClick={() => setShown(record)} className="flex w-full cursor-pointer items-start justify-between gap-3 rounded-lg border p-3 text-left hover:bg-muted focus-visible:outline-ring">
                  <span className="min-w-0">
                    <span className="block truncate text-sm font-medium">{record.projectName} · v{record.version}</span>
                    <span className="mt-0.5 block truncate text-xs text-muted-foreground">{record.branch} · {record.commit?.slice(0, 8) ?? "未记录提交"}</span>
                    <span className="mt-0.5 block truncate text-xs text-muted-foreground">{formatDate.format(record.startedAt)} · {duration(record.durationMs)}</span>
                  </span>
                  <ResultBadge success={record.success} />
                </button>
              ))}
            </div>
          </>
        )}
      </SheetContent>
    </Sheet>
  );
}
