import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDirectory } from "@tauri-apps/plugin-dialog";
import { toast } from "sonner";
import { FolderOpen, Loader2 } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import type { PackageManager, Project } from "@/types";
import { useProjectsStore } from "@/stores/projects";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/** Rust scan_projects 的返回结构 */
interface ScannedProject {
  path: string;
  inspection: {
    name: string;
    packageManager: PackageManager;
    script: string;
    framework: string;
  };
}

/** 目录扫描批量导入（PRD 开放问题 1）：选目录 → 扫一层子文件夹 → 勾选导入 */
export function ScanImportDialog({ open, onOpenChange }: Props) {
  const { projects, addMany } = useProjectsStore();

  const [root, setRoot] = useState("");
  const [scanning, setScanning] = useState(false);
  const [scanned, setScanned] = useState<ScannedProject[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [importing, setImporting] = useState(false);

  useEffect(() => {
    if (!open) {
      setRoot("");
      setScanned([]);
      setSelected(new Set());
    }
  }, [open]);

  async function pickAndScan() {
    const selected = await openDirectory({ directory: true, title: "选择要扫描的目录" });
    if (typeof selected !== "string") return;
    setRoot(selected);
    setScanning(true);
    setScanned([]);
    try {
      const found = await invoke<ScannedProject[]>("scan_projects", { root: selected });
      setScanned(found);
      // 默认全选未导入的项目
      const existing = new Set(projects.map((p) => p.path));
      setSelected(new Set(found.filter((f) => !existing.has(f.path)).map((f) => f.path)));
      toast.success(`扫描完成，发现 ${found.length} 个前端项目`);
    } catch (e) {
      toast.error(`扫描失败: ${e}`);
    } finally {
      setScanning(false);
    }
  }

  function toggle(path: string, checked: boolean) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (checked) next.add(path);
      else next.delete(path);
      return next;
    });
  }

  async function handleImport() {
    const targets = scanned.filter((s) => selected.has(s.path));
    if (targets.length === 0) return;
    setImporting(true);
    try {
      await addMany(targets.map((t): Project => ({
          id: crypto.randomUUID(),
          name: t.inspection.name,
          path: t.path,
          packageManager: t.inspection.packageManager,
          script: t.inspection.script,
          framework: t.inspection.framework as Project["framework"],
          createdAt: Date.now(),
      })));
      toast.success(`已导入 ${targets.length} 个项目`);
      onOpenChange(false);
    } catch (e) {
      toast.error(`导入失败: ${e}`);
    } finally {
      setImporting(false);
    }
  }

  const existing = new Set(projects.map((p) => p.path));

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>扫描目录导入项目</DialogTitle>
          <DialogDescription>
            扫描所选目录下一层的子文件夹，自动识别含 package.json 的前端项目。
          </DialogDescription>
        </DialogHeader>

        <div className="flex items-center gap-2">
          <Button variant="outline" size="sm" onClick={pickAndScan} disabled={scanning}>
            {scanning ? <Loader2 className="animate-spin" /> : <FolderOpen />}
            {scanning ? "扫描中…" : root ? "重新选择目录" : "选择目录"}
          </Button>
          {root && !scanning && (
            <span className="truncate text-sm text-muted-foreground">{root}</span>
          )}
        </div>

        {scanned.length > 0 && (
          <>
            <div className="max-h-72 overflow-y-auto rounded-lg border">
              {scanned.map((s) => {
                const already = existing.has(s.path);
                return (
                  <label
                    key={s.path}
                    className="flex cursor-pointer items-center gap-3 border-b px-3 py-2 last:border-b-0 hover:bg-muted/50"
                  >
                    <Checkbox
                      checked={selected.has(s.path)}
                      disabled={already}
                      onCheckedChange={(v) => toggle(s.path, v === true)}
                    />
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <span className="truncate text-sm font-medium">
                          {s.inspection.name}
                        </span>
                        {s.inspection.framework !== "other" && (
                          <Badge variant="outline" className="text-muted-foreground">
                            {s.inspection.framework}
                          </Badge>
                        )}
                        {already && (
                          <Badge variant="secondary">已在列表</Badge>
                        )}
                      </div>
                      <div className="truncate text-xs text-muted-foreground">
                        {s.path}
                      </div>
                    </div>
                    <span className="shrink-0 text-xs text-muted-foreground">
                      {s.inspection.packageManager} run {s.inspection.script || "?"}
                    </span>
                  </label>
                );
              })}
            </div>
            <div className="flex items-center justify-between">
              <span className="text-xs text-muted-foreground">
                已选 {selected.size} / {scanned.length} 个（已在列表的不可选）
              </span>
              <Button
                size="sm"
                onClick={handleImport}
                disabled={selected.size === 0 || importing}
              >
                {importing ? "导入中…" : `导入 ${selected.size} 个项目`}
              </Button>
            </div>
          </>
        )}

        {scanned.length === 0 && !scanning && root && (
          <p className="text-sm text-muted-foreground">
            该目录下一层没有找到含 package.json 的文件夹。
          </p>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            关闭
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
