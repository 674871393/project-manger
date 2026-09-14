import { useCallback, useEffect, useState } from "react";
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
import type { Project, ReleaseContext } from "@/types";
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
  { key: "precheck", label: "预检（工作区干净）" },
  { key: "checkout", label: "切换发布分支" },
  { key: "node", label: "切换 node 版本" },
  { key: "bump", label: "升版本号" },
  { key: "build", label: "执行构建" },
  { key: "zip", label: "压缩产物" },
  { key: "restore", label: "恢复 node 版本" },
];

/** 版本号字符白名单（与 Rust start_release 的校验一致，`cmd /C` 拼接安全） */
const VERSION_RE = /^[\w.\-+]+$/;

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

  const [ctx, setCtx] = useState<ReleaseContext | null>(null);
  const [ctxError, setCtxError] = useState<string | null>(null);
  const [bump, setBump] = useState<BumpKind>("patch");
  const [customVersion, setCustomVersion] = useState("");
  /** 是否已点过「开始发布」（控制弹窗处于配置态还是进度态） */
  const [started, setStarted] = useState(false);

  /** 读发布基准与工作区状态。返回修改 / 重试后也要重读——
   *  基准可能已变化（流水线会因此中止），拿旧值重试只会再失败一次 */
  const loadContext = useCallback(() => {
    if (!project?.release) return;
    setCtx(null);
    setCtxError(null);
    invoke<ReleaseContext>("read_release_context", {
      path: project.path,
      branch: project.release.branch,
    })
      .then((c) => {
        setCtx(c);
        setCtxError(null);
      })
      .catch((e) => {
        setCtx(null);
        setCtxError(String(e));
      });
  }, [project]);

  // 打开时读上下文、重置为配置态
  useEffect(() => {
    if (!open || !project) return;
    setStarted(false);
    setBump("patch");
    setCustomVersion("");
    loadContext();
  }, [open, project, loadContext]);

  // 进度态：本次点过开始，或流水线还在跑。后者是「后台运行」关掉弹窗后
  // 再点发布按钮回来的路径——此时必须直接看到进度，而不是一个「开始发布」
  // 被置灰、看不出所以然的配置表单
  const showProgress = (started || Boolean(run?.running)) && run !== undefined;
  const newVersion =
    bump === "custom"
      ? customVersion.trim()
      : ctx
        ? bumpVersion(ctx.releaseVersion, bump)
        : "";
  const canStart =
    newVersion.length > 0 &&
    VERSION_RE.test(newVersion) &&
    ctx !== null &&
    !run?.running;

  function handleStart() {
    if (!project || !canStart || !ctx) return;
    setStarted(true);
    startRelease(project, newVersion, ctx.releaseVersion);
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
            {/* 成功后版本号留在工作区未提交（PRD：不自动 commit），不提醒的话
                下次发布会被预检拦下，用户会莫名其妙 */}
            {run.zipPath && !run.running && (
              <p className="rounded-md bg-yellow-500/10 p-2 text-xs text-yellow-700 dark:text-yellow-400">
                package.json 版本号已更新且未提交，请记得 commit——工作区不干净时下次发布会被预检拦下
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
                  <Button
                    variant="outline"
                    onClick={() => {
                      setStarted(false);
                      // 重读基准：失败可能正是因为基准变了，拿旧值再点一次还会失败
                      loadContext();
                    }}
                  >
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
                <Label>发布分支版本</Label>
                <div className="rounded-md border px-3 py-2 font-mono text-sm text-muted-foreground">
                  {ctxError ? "读取失败" : (ctx?.releaseVersion ?? "读取中…")}
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
                {customVersion.trim() && !VERSION_RE.test(customVersion.trim()) && (
                  <p className="text-xs text-destructive">
                    版本号仅允许字母数字与 - _ . +
                  </p>
                )}
              </div>
            ) : (
              ctx && (
                <p className="text-sm text-muted-foreground">
                  {ctx.releaseVersion} →{" "}
                  <span className="font-mono text-foreground">{newVersion}</span>
                </p>
              )
            )}

            {/* 基准来自发布分支已提交的 package.json，与工作区当前检出/改了一半的
                那份可能不同，这里把两者都摆出来免得对不上号 */}
            {ctxError ? (
              <p className="rounded-md bg-destructive/10 p-2 text-xs text-destructive">
                {ctxError}
              </p>
            ) : (
              ctx && (
                <div className="grid gap-1.5 text-sm">
                  <div className="flex flex-wrap items-center gap-1.5">
                    <GitBranch className="size-3.5 text-muted-foreground" />
                    <span className="text-muted-foreground">当前分支</span>
                    <span className="font-mono">{ctx.currentBranch ?? "detached"}</span>
                    {ctx.currentBranch !== project?.release?.branch && (
                      <span className="text-muted-foreground">
                        开始后切到{" "}
                        <span className="font-mono">{project?.release?.branch}</span>
                      </span>
                    )}
                  </div>
                  {ctx.releaseRef !== project?.release?.branch && (
                    <p className="text-xs text-muted-foreground">
                      本地未检出 {project?.release?.branch}，基准取自 {ctx.releaseRef}
                    </p>
                  )}
                  {ctx.workingVersion && ctx.workingVersion !== ctx.releaseVersion && (
                    <p className="text-xs text-muted-foreground">
                      工作区当前是 {ctx.workingVersion}，发布以 {ctx.releaseVersion}{" "}
                      为基准（取发布分支上已提交的版本）
                    </p>
                  )}
                  {ctx.dirty && (
                    <p className="text-xs text-yellow-600 dark:text-yellow-400">
                      工作区有未提交变更，发布会先被预检拦下，请先提交或暂存
                    </p>
                  )}
                </div>
              )
            )}

            <p className="text-xs text-muted-foreground">
              将依次执行：切换到 {project?.release?.branch} 分支 → nvm use{" "}
              {project?.release?.nodeVersion} → 升版本 → {project?.release?.script}{" "}
              → 压缩 dist 为 zip（存放在项目 dist 目录）→ 恢复当前 node
              版本。版本基准取发布分支上已提交的 package.json；工作区有未提交变更会直接中止。
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
