import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Layers, Play, Plus, Save, Square, Trash2 } from "lucide-react";
import { toast } from "sonner";

import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle, AlertDialogTrigger } from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { describeStartResults } from "@/lib/start-results";
import { useRuntimeStore } from "@/stores/runtime";
import type { LaunchPlan, LaunchPlanConfig, Project } from "@/types";

interface Props {
  projects: Project[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function LaunchPlansDialog({ projects, open, onOpenChange }: Props) {
  const [config, setConfig] = useState<LaunchPlanConfig | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [projectIds, setProjectIds] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  function edit(plan?: LaunchPlan) {
    setEditingId(plan?.id ?? null);
    setName(plan?.name ?? "");
    setProjectIds(plan?.projectIds ?? []);
  }

  useEffect(() => {
    if (!open) return;
    let ignore = false;
    setConfig(null);
    setError(null);
    invoke<LaunchPlanConfig>("load_launch_plans").then((loaded) => {
      if (ignore) return;
      setConfig(loaded);
      edit(loaded.plans.find(({ id }) => id === loaded.lastUsedId) ?? loaded.plans[0]);
    }).catch((error) => { if (!ignore) setError(String(error)); });
    return () => { ignore = true; };
  }, [open]);

  const activePlan = config?.plans.find(({ id }) => id === editingId);
  const targets = projects.filter(({ id }) => activePlan?.projectIds.includes(id));
  const selected = projects.filter(({ id }) => projectIds.includes(id)).map(({ id }) => id);
  const edited = name.trim() !== activePlan?.name || projectIds.join("|") !== activePlan?.projectIds.join("|");

  async function persist(next: LaunchPlanConfig) {
    await invoke("save_launch_plans", { config: next });
    setConfig(next);
  }

  async function savePlan() {
    if (!config || !name.trim() || selected.length === 0) return;
    const plan = { id: editingId ?? crypto.randomUUID(), name: name.trim(), projectIds: selected };
    setBusy(true);
    try {
      const plans = editingId ? config.plans.map((item) => item.id === editingId ? plan : item) : [...config.plans, plan];
      await persist({ ...config, plans });
      edit(plan);
      toast.success("已保存启动方案");
    } catch (error) {
      toast.error("保存启动方案失败: " + error);
    } finally {
      setBusy(false);
    }
  }

  async function deletePlan() {
    if (!config || !editingId) return;
    setBusy(true);
    try {
      const plans = config.plans.filter(({ id }) => id !== editingId);
      await persist({ plans, lastUsedId: config.lastUsedId === editingId ? null : config.lastUsedId });
      edit(plans[0]);
    } catch (error) {
      toast.error("删除启动方案失败: " + error);
    } finally {
      setBusy(false);
    }
  }

  async function runPlan(action: "start" | "stop") {
    if (!config || !activePlan || targets.length === 0) return;
    setBusy(true);
    try {
      if (config.lastUsedId !== activePlan.id) await persist({ ...config, lastUsedId: activePlan.id });
      const runtime = useRuntimeStore.getState();
      if (action === "start") {
        const results = await Promise.all(targets.map(runtime.start));
        const summary = describeStartResults(results);
        if (results.includes("failed")) toast.warning(summary);
        else toast.info(summary);
      } else {
        const results = await Promise.all(targets.map(runtime.stop));
        const stopped = results.filter(Boolean).length;
        if (stopped === results.length) toast.success("已确认 " + stopped + " 个项目停止");
        else toast.warning("已确认停止 " + stopped + " 个，失败 " + (results.length - stopped) + " 个");
      }
      onOpenChange(false);
    } catch (error) {
      toast.error("执行方案失败: " + error);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={(next) => { if (!busy) onOpenChange(next); }}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2"><Layers className="size-4" />启动方案</DialogTitle>
          <DialogDescription>保存经常一起开发的项目，按方案启动或停止。</DialogDescription>
        </DialogHeader>
        {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : !config ? (
          <p className="text-sm text-muted-foreground">正在读取方案…</p>
        ) : (
          <div className="grid gap-4 sm:grid-cols-[11rem_1fr]">
            <div className="flex min-w-0 flex-col gap-1.5">
              <Button variant={editingId === null ? "secondary" : "ghost"} className="justify-start" disabled={busy} onClick={() => edit()}><Plus />新建方案</Button>
              <div className="max-h-72 overflow-auto">
                {config.plans.map((plan) => (
                  <Button key={plan.id} variant={plan.id === editingId ? "secondary" : "ghost"} className="mb-1 w-full justify-start" disabled={busy} onClick={() => edit(plan)}>
                    <span className="truncate" title={plan.name}>{plan.name}</span>
                    <span className="ml-auto text-xs tabular-nums text-muted-foreground">{plan.projectIds.length}</span>
                  </Button>
                ))}
              </div>
            </div>
            <div className="grid min-w-0 gap-3">
              <div className="grid gap-1.5">
                <Label htmlFor="launch-plan-name">方案名称</Label>
                <Input id="launch-plan-name" value={name} disabled={busy} maxLength={80} placeholder="例如：后台联调" onChange={(event) => setName(event.currentTarget.value)} />
              </div>
              <div className="max-h-52 overflow-auto rounded-lg border p-2" aria-label="方案包含的项目">
                {projects.length === 0 && <p className="p-2 text-sm text-muted-foreground">添加项目后即可创建方案。</p>}
                {projects.map((project) => (
                  <label key={project.id} className="flex cursor-pointer items-center gap-2 rounded px-2 py-1.5 text-sm hover:bg-muted">
                    <Checkbox checked={projectIds.includes(project.id)} disabled={busy} onCheckedChange={(checked) => {
                      setProjectIds((current) => checked === true ? [...current, project.id] : current.filter((id) => id !== project.id));
                    }} />
                    <span className="truncate" title={project.path}>{project.name}</span>
                  </label>
                ))}
              </div>
              {projectIds.length > selected.length && <p className="text-xs text-muted-foreground">方案包含 {projectIds.length - selected.length} 个已移除的项目，保存时会更新。</p>}
              <div className="flex flex-wrap items-center gap-2">
                <Button disabled={busy || !name.trim() || selected.length === 0 || !edited} onClick={savePlan}><Save />保存方案</Button>
                <Button variant="outline" disabled={busy || edited || targets.length === 0} onClick={() => runPlan("start")}><Play />启动</Button>
                <Button variant="outline" disabled={busy || edited || targets.length === 0} onClick={() => runPlan("stop")}><Square />停止</Button>
                {editingId && (
                  <AlertDialog>
                    <AlertDialogTrigger asChild><Button variant="ghost" size="icon" className="ml-auto text-destructive" disabled={busy} aria-label="删除方案"><Trash2 /></Button></AlertDialogTrigger>
                    <AlertDialogContent>
                      <AlertDialogHeader><AlertDialogTitle>删除「{activePlan?.name}」？</AlertDialogTitle><AlertDialogDescription>此操作会移除这条启动方案。</AlertDialogDescription></AlertDialogHeader>
                      <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel><AlertDialogAction onClick={deletePlan}>删除</AlertDialogAction></AlertDialogFooter>
                    </AlertDialogContent>
                  </AlertDialog>
                )}
              </div>
            </div>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
