import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDirectory } from "@tauri-apps/plugin-dialog";
import { toast } from "sonner";
import { FolderOpen, Plus, X } from "lucide-react";

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
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import type {
  Framework,
  PackageManager,
  Project,
  ProjectInspection,
  ReleaseConfig,
} from "@/types";
import { useProjectsStore } from "@/stores/projects";

interface Props {
  /** 编辑模式传入已有项目；添加模式传 null */
  initial: Project | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

const PACKAGE_MANAGERS: PackageManager[] = ["npm", "pnpm", "yarn", "bun"];

/** 添加/编辑项目共用的表单对话框（PRD FR-1） */
export function ProjectFormDialog({ initial, open, onOpenChange }: Props) {
  const isEdit = initial !== null;
  const { projects, add, update } = useProjectsStore();

  const [name, setName] = useState("");
  const [path, setPath] = useState("");
  const [packageManager, setPackageManager] = useState<PackageManager>("npm");
  const [script, setScript] = useState("dev");
  const [manualPort, setManualPort] = useState("");
  /** inspect_project 识别出的框架（仅展示标签用，不提供手动编辑） */
  const [framework, setFramework] = useState<Framework | undefined>(undefined);
  /** 环境变量行（空 key 的行提交时丢弃） */
  const [envRows, setEnvRows] = useState<{ key: string; value: string }[]>([]);
  /** 发布配置（FR-20，可选启用） */
  const [releaseEnabled, setReleaseEnabled] = useState(false);
  const [releaseBranch, setReleaseBranch] = useState("");
  const [releaseNode, setReleaseNode] = useState("");
  const [releaseScript, setReleaseScript] = useState("build:prod");
  const [releaseOutputDir, setReleaseOutputDir] = useState("dist");
  const [versionLogMode, setVersionLogMode] = useState<"auto" | "off" | "custom">("auto");
  const [versionLogPath, setVersionLogPath] = useState("");
  /** 预检额外忽略，一行一个。.env.development 始终忽略 */
  const [releaseIgnore, setReleaseIgnore] = useState("");
  const [submitting, setSubmitting] = useState(false);

  // 打开时用 initial 重置表单
  useEffect(() => {
    if (open) {
      setName(initial?.name ?? "");
      setPath(initial?.path ?? "");
      setPackageManager(initial?.packageManager ?? "npm");
      setScript(initial?.script ?? "dev");
      setManualPort(initial?.manualPort ? String(initial.manualPort) : "");
      setFramework(initial?.framework);
      setEnvRows(
        initial?.env
          ? Object.entries(initial.env).map(([key, value]) => ({ key, value }))
          : [],
      );
      setReleaseEnabled(Boolean(initial?.release));
      setReleaseBranch(initial?.release?.branch ?? "");
      setReleaseNode(initial?.release?.nodeVersion ?? "");
      setReleaseScript(initial?.release?.script ?? "build:prod");
      setReleaseOutputDir(initial?.release?.outputDir ?? "dist");
      const logPath = initial?.release?.versionLogPath;
      setVersionLogMode(logPath === undefined ? "auto" : logPath === "" ? "off" : "custom");
      setVersionLogPath(logPath ?? "");
      setReleaseIgnore((initial?.release?.ignorePaths ?? []).join("\n"));
    }
  }, [open, initial]);

  async function pickFolder() {
    const selected = await openDirectory({ directory: true, title: "选择项目文件夹" });
    if (typeof selected !== "string") return;
    try {
      const inspection = await invoke<ProjectInspection>("inspect_project", {
        path: selected,
      });
      setPath(selected);
      setName(inspection.name);
      setPackageManager(inspection.packageManager);
      setScript(inspection.script);
      setFramework(inspection.framework);
      toast.success(`已识别 ${inspection.name}（${inspection.framework}）`);
    } catch (e) {
      // 保留已选路径，让用户能手动填完（PRD：路径不对可编辑改）
      setPath(selected);
      toast.error(String(e));
    }
  }

  async function handleSubmit() {
    if (!path.trim()) {
      toast.error("请先选择项目文件夹");
      return;
    }
    if (!name.trim()) {
      toast.error("项目名称不能为空");
      return;
    }
    // 同一路径禁止重复添加
    if (projects.some((p) => p.path === path && p.id !== initial?.id)) {
      toast.error("该项目已在列表中");
      return;
    }

    const port = manualPort.trim() ? Number(manualPort) : undefined;
    if (port !== undefined && (!Number.isInteger(port) || port < 1 || port > 65535)) {
      toast.error("端口号必须是 1-65535 的整数");
      return;
    }

    // 发布配置：启用即三项都必填（半填的配置没法执行流水线）
    let release: ReleaseConfig | undefined;
    if (releaseEnabled) {
      if (!releaseBranch.trim() || !releaseNode.trim() || !releaseScript.trim()) {
        toast.error("发布配置需要完整填写：分支、node 版本、构建脚本");
        return;
      }
      // 命令在 Rust 侧是 `cmd /C` 拼接执行的：含空格或 & | 等元字符会被拆成
      // 别的命令，这里与 start_release 的白名单保持一致（分支额外允许 /）
      if (!/^[\w\-./:+@]+$/.test(releaseBranch.trim())) {
        toast.error("发布分支含非法字符（仅允许字母数字与 - _ . /）");
        return;
      }
      if (!/^\d+\.\d+\.\d+$/.test(releaseNode.trim())) {
        toast.error("node 版本含非法字符（应为形如 16.20.0 的完整版本号）");
        return;
      }
      if (!/^[\w\-.:+@]+$/.test(releaseScript.trim())) {
        toast.error("构建脚本名含非法字符（仅允许字母数字与 - _ . : @ +）");
        return;
      }
      const outputDir = releaseOutputDir.trim() || "dist";
      const parts = outputDir.replace(/\\/g, "/").split("/");
      if (parts.some((part) => !part || /[<>:"|?*]/.test(part) || /[. ]$/.test(part))) {
        toast.error("产物目录须为项目下的相对路径，例如 dist 或 .output/public");
        return;
      }
      const logPath = versionLogPath.trim().replace(/\\/g, "/");
      if (versionLogMode === "custom" && (
        !logPath.toLowerCase().endsWith(".json") ||
        logPath.split("/").some((part) => !part || part === "." || part === ".." || /[<>:"|?*\u0000-\u001f]/.test(part) || /[. ]$/.test(part)) ||
        logPath.toLowerCase() === "package.json"
      )) {
        toast.error("请输入项目内的版本日志 JSON 路径，例如 src/config/versionLog.json");
        return;
      }
      const ignorePaths = releaseIgnore
        .split(/[\n,]/)
        .map((s) => s.trim())
        .filter(Boolean);
      const tooBroad = ignorePaths.find((p) =>
        ["*", "**", "**/*", "**/**"].includes(p.replace(/\\/g, "/")),
      );
      if (tooBroad) {
        toast.error(`忽略规则「${tooBroad}」会放过所有文件，请改成具体路径`);
        return;
      }
      release = {
        branch: releaseBranch.trim(),
        nodeVersion: releaseNode.trim(),
        script: releaseScript.trim(),
        outputDir,
        ...(versionLogMode !== "auto" ? { versionLogPath: versionLogMode === "off" ? "" : logPath } : {}),
        ...(ignorePaths.length > 0 ? { ignorePaths } : {}),
      };
    }

    setSubmitting(true);
    try {
      const env: Record<string, string> | undefined = (() => {
        const entries = envRows
          .filter((r) => r.key.trim() !== "")
          .map((r) => [r.key.trim(), r.value] as const);
        return entries.length > 0 ? Object.fromEntries(entries) : undefined;
      })();

      const project: Project = {
        id: initial?.id ?? crypto.randomUUID(),
        name: name.trim(),
        path,
        packageManager,
        script: script.trim(),
        manualPort: port,
        env,
        framework,
        release,
        createdAt: initial?.createdAt ?? Date.now(),
      };
      if (isEdit) {
        await update(project);
        toast.success(`已更新「${project.name}」`);
      } else {
        await add(project);
        toast.success(`已添加「${project.name}」`);
      }
      onOpenChange(false);
    } catch (e) {
      toast.error(`保存失败: ${e}`);
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{isEdit ? "编辑项目" : "添加项目"}</DialogTitle>
          <DialogDescription>
            选择本地项目文件夹，自动识别 package.json 中的配置。
          </DialogDescription>
        </DialogHeader>

        <div className="grid gap-4">
          <div className="grid gap-2">
            <Label htmlFor="project-path">项目路径</Label>
            <div className="flex gap-2">
              <Input
                id="project-path"
                value={path}
                readOnly
                placeholder="点击右侧按钮选择文件夹"
                className="flex-1"
              />
              <Button variant="outline" size="icon" onClick={pickFolder} title="选择文件夹">
                <FolderOpen />
              </Button>
            </div>
          </div>

          <div className="grid gap-2">
            <Label htmlFor="project-name">项目名称</Label>
            <Input
              id="project-name"
              value={name}
              onChange={(e) => setName(e.currentTarget.value)}
              placeholder="默认取 package.json 的 name"
            />
          </div>

          <div className="grid grid-cols-2 gap-4">
            <div className="grid gap-2">
              <Label>包管理器</Label>
              <Select
                value={packageManager}
                onValueChange={(v) => setPackageManager(v as PackageManager)}
              >
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {PACKAGE_MANAGERS.map((pm) => (
                    <SelectItem key={pm} value={pm}>
                      {pm}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>

            <div className="grid gap-2">
              <Label htmlFor="project-script">启动脚本</Label>
              <Input
                id="project-script"
                value={script}
                onChange={(e) => setScript(e.currentTarget.value)}
                placeholder="dev / start"
              />
            </div>
          </div>

          <div className="grid gap-2">
            <Label htmlFor="project-port">手动端口（可选）</Label>
            <Input
              id="project-port"
              inputMode="numeric"
              value={manualPort}
              onChange={(e) => setManualPort(e.currentTarget.value)}
              placeholder="日志解析不出端口时兜底，如 5173"
            />
          </div>

          <div className="grid gap-2">
            <Label>环境变量（可选）</Label>
            {envRows.length === 0 && (
              <Button
                variant="outline"
                size="sm"
                className="w-fit"
                onClick={() => setEnvRows([{ key: "", value: "" }])}
              >
                <Plus />
                添加变量
              </Button>
            )}
            <div className="grid gap-2">
              {envRows.map((row, i) => (
                <div key={i} className="flex items-center gap-2">
                  <Input
                    value={row.key}
                    onChange={(e) =>
                      setEnvRows((rows) =>
                        rows.map((r, j) =>
                          j === i ? { ...r, key: e.currentTarget.value } : r,
                        ),
                      )
                    }
                    placeholder="变量名，如 PORT"
                    className="flex-1 font-mono text-xs"
                  />
                  <Input
                    value={row.value}
                    onChange={(e) =>
                      setEnvRows((rows) =>
                        rows.map((r, j) =>
                          j === i ? { ...r, value: e.currentTarget.value } : r,
                        ),
                      )
                    }
                    placeholder="值"
                    className="flex-1 font-mono text-xs"
                  />
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label="删除此变量"
                    onClick={() =>
                      setEnvRows((rows) => rows.filter((_, j) => j !== i))
                    }
                  >
                    <X />
                  </Button>
                </div>
              ))}
              {envRows.length > 0 && (
                <Button
                  variant="outline"
                  size="sm"
                  className="w-fit"
                  onClick={() => setEnvRows((rows) => [...rows, { key: "", value: "" }])}
                >
                  <Plus />
                  再加一条
                </Button>
              )}
            </div>
          </div>

          <div className="grid gap-2 rounded-lg border p-3">
            <label className="flex items-center gap-2 text-sm font-medium">
              <Checkbox
                checked={releaseEnabled}
                onCheckedChange={(v) => setReleaseEnabled(v === true)}
                aria-label="启用一键发布"
              />
              发布配置（一键发版：切分支 → 切 node → 升版本 → build → zip）
            </label>
            {releaseEnabled && (
              <div className="grid gap-2 pl-6">
                <div className="grid grid-cols-2 gap-4">
                  <div className="grid gap-2">
                    <Label htmlFor="release-branch">发布分支</Label>
                    <Input
                      id="release-branch"
                      value={releaseBranch}
                      onChange={(e) => setReleaseBranch(e.currentTarget.value)}
                      placeholder="如 release / master"
                    />
                  </div>
                  <div className="grid gap-2">
                    <Label htmlFor="release-node">node 版本</Label>
                    <Input
                      id="release-node"
                      value={releaseNode}
                      onChange={(e) => setReleaseNode(e.currentTarget.value)}
                      placeholder="nvm 完整版本号，如 16.20.0"
                    />
                  </div>
                </div>
                <div className="grid gap-2">
                  <Label htmlFor="release-script">构建脚本</Label>
                  <Input
                    id="release-script"
                    value={releaseScript}
                    onChange={(e) => setReleaseScript(e.currentTarget.value)}
                    placeholder="package.json scripts 里的脚本名，如 build:prod"
                  />
                </div>
                <div className="grid gap-2">
                  <Label htmlFor="release-output-dir">产物目录</Label>
                  <Input id="release-output-dir" value={releaseOutputDir} onChange={(e) => setReleaseOutputDir(e.currentTarget.value)} placeholder="dist" />
                  <p className="text-xs text-muted-foreground">相对于项目目录。构建完成后检查新产物，ZIP 保存在此目录。</p>
                </div>
                <div className="grid gap-2">
                  <Label htmlFor="release-version-log-mode">发布公告</Label>
                  <Select value={versionLogMode} onValueChange={(value) => setVersionLogMode(value as typeof versionLogMode)}>
                    <SelectTrigger id="release-version-log-mode" className="w-full"><SelectValue /></SelectTrigger>
                    <SelectContent>
                      <SelectItem value="auto">自动识别 src/config/versionLog.json</SelectItem>
                      <SelectItem value="custom">指定版本日志</SelectItem>
                      <SelectItem value="off">关闭同步</SelectItem>
                    </SelectContent>
                  </Select>
                  {versionLogMode === "custom" && (
                    <Input aria-label="版本日志相对路径" value={versionLogPath} onChange={(e) => setVersionLogPath(e.currentTarget.value)} placeholder="src/config/versionLog.json" />
                  )}
                </div>
                <p className="text-xs text-muted-foreground">
                  发布前检查 Git 工作区。指定的 Node 版本仅用于本次构建子进程，版本更新需自行提交。
                </p>
                <div className="grid gap-2">
                  <Label htmlFor="release-ignore">预检忽略的文件</Label>
                  <textarea
                    id="release-ignore"
                    value={releaseIgnore}
                    onChange={(e) => setReleaseIgnore(e.currentTarget.value)}
                    placeholder={"每行一个，如\n.env.local\n*.local"}
                    rows={3}
                    className="w-full rounded-lg border border-input bg-transparent px-2.5 py-1.5 font-mono text-sm outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50"
                  />
                  <p className="text-xs text-muted-foreground">
                    .env.development 始终忽略。pnpm-workspace.yaml 若只有 allowBuilds
                    变化，发布弹窗会显示差异，规则保留在项目配置中。
                  </p>
                </div>
              </div>
            )}
          </div>
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            取消
          </Button>
          <Button onClick={handleSubmit} disabled={submitting}>
            {submitting ? "保存中…" : isEdit ? "保存" : "添加"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
