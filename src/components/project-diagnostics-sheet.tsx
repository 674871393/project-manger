import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Activity, Copy, RefreshCw } from "lucide-react";
import { toast } from "sonner";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import type { PortOwner, Project, ProjectDiagnostics } from "@/types";

interface Props {
  project: Project | null;
  projects: Project[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

function Check({ title, value, ok = true }: { title: string; value: string; ok?: boolean }) {
  return (
    <div className="grid grid-cols-[7rem_1fr] items-baseline gap-2 py-1 text-sm">
      <dt className="text-muted-foreground">{title}</dt>
      <dd className={ok ? "break-all" : "break-all text-destructive"}>{value}</dd>
    </div>
  );
}

function EnvironmentChecks({ data, project }: { data: ProjectDiagnostics; project: Project }) {
  const manager = "Ok" in data.packageManager ? project.packageManager + " " + data.packageManager.Ok : data.packageManager.Err;
  return (
    <dl className="rounded-lg border p-3">
      <Check title="项目目录" value={data.directoryExists ? project.path : "目录不存在"} ok={data.directoryExists} />
      <Check title="package.json" value={data.packageExists ? "可读取" : "缺失或格式错误"} ok={data.packageExists} />
      <Check title="Node" value={"Ok" in data.node ? data.node.Ok : data.node.Err} ok={"Ok" in data.node} />
      <Check title="包管理器" value={manager} ok={"Ok" in data.packageManager} />
      {data.detectedPackageManager && data.detectedPackageManager !== project.packageManager && (
        <Check title="项目声明" value={data.detectedPackageManager + "，与项目配置不同"} />
      )}
      <Check title="依赖" value={data.dependenciesAvailable ? "可用" : "未检测到已安装依赖"} ok={data.dependenciesAvailable} />
      <Check title="启动脚本" value={data.scriptExists ? project.script : "未在 package.json 找到 " + project.script} ok={data.scriptExists} />
      <Check title="Git 分支" value={data.gitBranch ?? "未检测到"} />
      {data.workspaceRoot && <Check title="工作区根目录" value={data.workspaceRoot} />}
    </dl>
  );
}

export function ProjectDiagnosticsSheet({ project, projects, open, onOpenChange }: Props) {
  const [data, setData] = useState<ProjectDiagnostics | null>(null);
  const [ports, setPorts] = useState<PortOwner[] | null>(null);
  const [diagnosticError, setDiagnosticError] = useState<string | null>(null);
  const [portError, setPortError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [refresh, setRefresh] = useState(0);

  useEffect(() => {
    if (open) setQuery("");
  }, [open, project]);

  useEffect(() => {
    if (!open) return;
    let ignore = false;
    setData(null);
    setPorts(null);
    setDiagnosticError(null);
    setPortError(null);
    if (project) {
      invoke<ProjectDiagnostics>("diagnose_project", { project }).then((value) => {
        if (!ignore) setData(value);
      }).catch((error) => { if (!ignore) setDiagnosticError(String(error)); });
    }
    invoke<PortOwner[]>("list_listening_ports").then((value) => {
      if (!ignore) setPorts(value);
    }).catch((error) => { if (!ignore) setPortError(String(error)); });
    return () => { ignore = true; };
  }, [open, project, refresh]);

  const filter = query.trim();
  const defaultPort = project?.manualPort;
  const shown = ports?.filter((entry) => {
    if (filter) return String(entry.port).includes(filter);
    if (project) return entry.projectId === project.id || entry.port === defaultPort;
    return true;
  });
  const projectsById = new Map(projects.map(({ id, name }) => [id, name]));

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent className="gap-0" style={{ width: "min(100%, 36rem)", maxWidth: "none" }}>
        <SheetHeader className="border-b pr-12">
          <SheetTitle className="flex items-center gap-2"><Activity className="size-4" />{project ? "项目体检" : "端口总览"}</SheetTitle>
          <SheetDescription>{project ? project.name : "查询本机 TCP 监听端口及占用进程。"}</SheetDescription>
        </SheetHeader>
        <div className="min-h-0 flex-1 space-y-4 overflow-auto p-4">
          {project && (data ? <EnvironmentChecks data={data} project={project} /> : (
            <p role={diagnosticError ? "alert" : undefined} className={diagnosticError ? "text-sm text-destructive" : "text-sm text-muted-foreground"}>
              {diagnosticError ?? "正在检查项目环境…"}
            </p>
          ))}
          <div className="flex flex-wrap items-center gap-2">
            <h3 className="mr-auto text-sm font-medium">监听端口</h3>
            <Input value={query} inputMode="numeric" maxLength={5} className="w-32" placeholder="搜索端口" aria-label="搜索端口" onChange={(event) => setQuery(event.currentTarget.value.replace(/[^0-9]/g, ""))} />
            <Button variant="outline" size="icon" onClick={() => setRefresh((value) => value + 1)} aria-label="重新检查"><RefreshCw /></Button>
          </div>
          {portError && <p role="alert" className="text-sm text-destructive">{portError}</p>}
          {!ports && !portError && <p className="text-sm text-muted-foreground">正在读取端口…</p>}
          {shown?.length === 0 && <p className="rounded-lg border border-dashed p-4 text-sm text-muted-foreground">未发现匹配的监听端口。</p>}
          {shown?.map((entry) => {
            const owner = entry.projectId ? projectsById.get(entry.projectId) ?? "本工具启动的进程" : "外部进程";
            const address = (entry.address.includes(":") ? "[" + entry.address + "]" : entry.address) + ":" + entry.port;
            return (
              <div key={entry.pid + "/" + address} className="flex items-center justify-between gap-2 rounded-lg border px-3 py-2">
                <div className="min-w-0">
                  <p className="truncate font-mono text-sm">{address}</p>
                  <p className="truncate text-xs text-muted-foreground">{entry.process ?? "未知进程"} · PID {entry.pid}</p>
                </div>
                <Badge variant="secondary" className="max-w-36 truncate" title={owner}>{owner}</Badge>
                <Button variant="ghost" size="icon-sm" aria-label={"复制 PID " + entry.pid} onClick={async () => {
                  try { await navigator.clipboard.writeText(String(entry.pid)); toast.success("已复制 PID"); }
                  catch (error) { toast.error("复制 PID 失败: " + error); }
                }}><Copy /></Button>
              </div>
            );
          })}
        </div>
      </SheetContent>
    </Sheet>
  );
}
