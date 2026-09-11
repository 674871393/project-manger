import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { toast } from "sonner";
import {
  CircleCheck,
  CircleX,
  FolderOpen,
  GitBranch,
  Loader2,
  Rocket,
  RotateCcw,
  Circle,
} from "lucide-react";

import { Button } from "@/components/ui/button";
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
import type { Project } from "@/types";
import {
  useRuntimeStore,
  type ReleaseStep,
  type ReleaseStepStatus,
} from "@/stores/runtime";

interface Props {
  project: Project | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

type BumpKind = "patch" | "minor" | "major" | "custom";

const STEP_LABELS: { key: ReleaseStep; label: string }[] = [
  { key: "checkout", label: "切换发布分支" },
  { key: "node", label: "切换 node 版本" },
  { key: "bump", label: "升版本号" },
  { key: "build", label: "执行构建" },
  { key: "zip", label: "压缩产物" },
  { key: "restore", label: "恢复 node 版本" },
];

/** 版本号 +1（PRD FR-20）：
 * patch = 最后一段 +1，兼容任意段数（1.2.4 → 1.2.5，1.2.0.13 → 1.2.0.14）；
 * minor / major 仅对标准三段版本有意义（1.2.4 → 1.3.0 / 2.0.0），其他段数退化为最后一段 +1；
 * 含非数字段（如 1.2.4-rc.1 拆出的段）返回原值，需手填 */
function bumpVersion(v: string, kind: BumpKind): string {
  const parts = v.split(".");
  if (parts.length < 2 || parts.some((p) => !/^\d+$/.test(p))) return v;
  const nums = parts.map(Number);
  if (kind === "patch" || parts.length !== 3) {
    nums[nums.length - 1] += 1;
  } else if (kind === "minor") {
    nums[1] += 1;
    nums[2] = 0;
  } else {
    nums[0] += 1;
    nums[1] = 0;
    nums[2] = 0;
  }
  return nums.join(".");
}

function StepIcon({ status }: { status?: ReleaseStepStatus }) {
  if (status === "running")
    return <Loader2 className="size-4 animate-spin text-blue-500" />;
  if (status === "success")
    return <CircleCheck className="size-4 text-emerald-500" />;
  if (status === "error") return <CircleX className="size-4 text-destructive" />;
  return <Circle className="size-4 text-muted-foreground/40" />;
}

/**
 * 一键发布弹窗（PRD FR-20）：确认新版本号 → 六步流水线实时进度。
 * 发布日志走统一日志抽屉（[release] 前缀），失败时由 store 自动弹出。
 */
export function ReleaseDialog({ project, open, onOpenChange }: Props) {
  const startRelease = useRuntimeStore((s) => s.startRelease);
  const releaseRuns = useRuntimeStore((s) => s.releaseRuns);
  const run = project ? releaseRuns[project.id] : undefined;

  const [currentVersion, setCurrentVersion] = useState<string | null>(null);
  /** 当前 git 分支（发布前确认是否已在打包分支上） */
  const [currentBranch, setCurrentBranch] = useState<string | null>(null);
  const [bump, setBump] = useState<BumpKind>("patch");
  const [customVersion, setCustomVersion] = useState("");
  /** 是否已点过「开始发布」（控制弹窗处于配置态还是进度态） */
  const [started, setStarted] = useState(false);

  // 打开时读当前版本与当前分支、重置为配置态
  useEffect(() => {
    if (!open || !project) return;
    setStarted(false);
    setBump("patch");
    setCustomVersion("");
    setCurrentVersion(null);
    setCurrentBranch(null);
    invoke<string>("read_package_version", { path: project.path })
      .then(setCurrentVersion)
      .catch((e) => {
        setCurrentVersion(null);
        toast.error(String(e));
      });
    invoke<string | null>("read_git_branch", { path: project.path })
      .then(setCurrentBranch)
      .catch(() => setCurrentBranch(null));
  }, [open, project]);

  // 上次发布已结束 → 本次打开回到配置态（started 标记重置过）
  const showProgress = started && run !== undefined;
  const newVersion =
    bump === "custom"
      ? customVersion.trim()
      : currentVersion
        ? bumpVersion(currentVersion, bump)
        : "";
  const canStart = newVersion.length > 0 && !run?.running;

  function handleStart() {
    if (!project || !canStart) return;
    setStarted(true);
    startRelease(project, newVersion);
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Rocket className="size-4" />
            发布「{project?.name}」
          </DialogTitle>
          <DialogDescription>
            {project?.release
              ? `分支 ${project.release.branch} · node ${project.release.nodeVersion} · ${project.release.script}`
              : "未配置发布参数"}
          </DialogDescription>
        </DialogHeader>

        {showProgress ? (
          <div className="grid gap-4">
            <div className="grid gap-2">
              {STEP_LABELS.map(({ key, label }) => (
                <div key={key} className="flex items-center gap-2 text-sm">
                  <StepIcon status={run.steps[key]} />
                  <span
                    className={
                      run.steps[key] === "running" ? "font-medium" : "text-muted-foreground"
                    }
                  >
                    {label}
                  </span>
                  {run.messages[key] && (
                    <span className="ml-auto truncate text-xs text-muted-foreground">
                      {run.messages[key]}
                    </span>
                  )}
                </div>
              ))}
            </div>

            {run.error && (
              <p className="rounded-md bg-destructive/10 p-2 text-sm text-destructive">
                {run.error}
                {"　失败详情见日志抽屉（[release] 前缀）"}
              </p>
            )}
            {run.zipPath && (
              <p className="rounded-md bg-emerald-500/10 p-2 font-mono text-xs break-all text-emerald-700 dark:text-emerald-400">
                {run.zipPath}
              </p>
            )}

            <DialogFooter>
              {run.zipPath && (
                <Button
                  variant="outline"
                  onClick={() =>
                    revealItemInDir(run.zipPath!).catch((e) =>
                      toast.error(`打开文件夹失败: ${e}`),
                    )
                  }
                >
                  <FolderOpen />
                  打开所在文件夹
                </Button>
              )}
              {run.error && !run.running && (
                <>
                  <Button variant="outline" onClick={() => setStarted(false)}>
                    返回修改
                  </Button>
                  <Button onClick={handleStart} disabled={!canStart}>
                    <RotateCcw />
                    重试
                  </Button>
                </>
              )}
              <Button onClick={() => onOpenChange(false)}>
                {run.running ? "后台运行" : "完成"}
              </Button>
            </DialogFooter>
          </div>
        ) : (
          <div className="grid gap-4">
            <div className="grid grid-cols-2 gap-4">
              <div className="grid gap-2">
                <Label>当前版本</Label>
                <div className="rounded-md border px-3 py-2 font-mono text-sm text-muted-foreground">
                  {currentVersion ?? "读取中…"}
                </div>
              </div>
              <div className="grid gap-2">
                <Label>新版本</Label>
                <Select
                  value={bump}
                  onValueChange={(v) => setBump(v as BumpKind)}
                >
                  <SelectTrigger className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="patch">patch +1</SelectItem>
                    <SelectItem value="minor">minor</SelectItem>
                    <SelectItem value="major">major</SelectItem>
                    <SelectItem value="custom">手动指定</SelectItem>
                  </SelectContent>
                </Select>
              </div>
            </div>

            {bump === "custom" ? (
              <div className="grid gap-2">
                <Label htmlFor="release-custom-version">手动指定版本号</Label>
                <Input
                  id="release-custom-version"
                  value={customVersion}
                  onChange={(e) => setCustomVersion(e.currentTarget.value)}
                  placeholder="如 2.0.0-rc.1"
                  className="font-mono"
                />
              </div>
            ) : (
              currentVersion && (
                <p className="text-sm text-muted-foreground">
                  {currentVersion} →{" "}
                  <span className="font-mono text-foreground">{newVersion}</span>
                </p>
              )
            )}

            {/* 发布前确认当前是否已在打包分支（未在则开始时自动切换） */}
            <div className="flex flex-wrap items-center gap-1.5 text-sm">
              <GitBranch className="size-3.5 text-muted-foreground" />
              <span className="text-muted-foreground">当前分支</span>
              <span className="font-mono">{currentBranch ?? "-"}</span>
              {project?.release &&
                currentBranch &&
                currentBranch !== project.release.branch && (
                  <span className="text-yellow-600 dark:text-yellow-400">
                    不在发布分支 {project.release.branch}，开始后将自动切换
                  </span>
                )}
            </div>

            <p className="text-xs text-muted-foreground">
              将依次执行：切换到 {project?.release?.branch} 分支 → nvm use{" "}
              {project?.release?.nodeVersion} → 升版本 → {project?.release?.script}{" "}
              → 压缩 dist 为 zip（存放在项目 dist 目录）→ 恢复当前 node
              版本。工作区有未提交变更会直接中止。
            </p>

            <DialogFooter>
              <Button variant="outline" onClick={() => onOpenChange(false)}>
                取消
              </Button>
              <Button onClick={handleStart} disabled={!canStart}>
                <Rocket />
                开始发布
              </Button>
            </DialogFooter>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
