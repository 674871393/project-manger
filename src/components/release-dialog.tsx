import { useCallback, useEffect, useRef, useState } from "react";
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

interface AnnouncementDraft {
  date: string;
  notes: string;
}

const STEP_LABELS: { key: ReleaseStep; label: string }[] = [
  { key: "precheck", label: "预检（工作区干净）" },
  { key: "node", label: "准备构建使用的 Node" },
  { key: "checkout", label: "切换发布分支" },
  { key: "bump", label: "升版本号" },
  { key: "build", label: "执行构建" },
  { key: "zip", label: "压缩产物" },
];

/** 版本号字符白名单（与 Rust start_release 的校验一致，`cmd /C` 拼接安全） */
const VERSION_RE = /^\d(?:[A-Za-z0-9.\-+]*[A-Za-z0-9])?$/;

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
  const [announcementDrafts, setAnnouncementDrafts] = useState<Record<string, AnnouncementDraft>>({});
  /** 是否已点过「开始发布」（控制弹窗处于配置态还是进度态） */
  const [started, setStarted] = useState(false);
  const contextRequest = useRef(0);
  const anotherRelease = Object.entries(releaseRuns).some(([id, state]) => id !== project?.id && state.running);

  /** 读发布基准与工作区状态。返回修改 / 重试后也要重读——
   *  基准可能已变化（流水线会因此中止），拿旧值重试只会再失败一次 */
  const loadContext = useCallback(() => {
    if (!project?.release) return;
    const request = ++contextRequest.current;
    setCtx(null);
    setCtxError(null);
    invoke<ReleaseContext>("read_release_context", {
      path: project.path,
      branch: project.release.branch,
      ignorePaths: project.release.ignorePaths ?? [],
      versionLogPath: project.release.versionLogPath,
    })
      .then((c) => {
        if (contextRequest.current !== request) return;
        setCtx(c);
        setCtxError(null);
      })
      .catch((e) => {
        if (contextRequest.current !== request) return;
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
    setAnnouncementDrafts({});
    loadContext();
    return () => { contextRequest.current++; };
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
  const blocking = ctx?.blocking ?? [];
  const versionLog = ctx?.versionLog;
  const existing = versionLog?.latest?.version === newVersion ? versionLog.latest : null;
  const archivedVersion = !existing && versionLog?.versions.includes(newVersion);
  const draftKey = versionLog?.revision + "/" + newVersion;
  const draft = announcementDrafts[draftKey] ?? {
    date: existing?.date ?? ctx?.releaseDate ?? "",
    notes: existing?.list.join("\n") ?? "",
  };
  const announcementLines = draft.notes.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
  const announcementReady = !versionLog || Boolean(draft.date && announcementLines.length && !archivedVersion);
  const canStart =
    newVersion.length > 0 &&
    VERSION_RE.test(newVersion) &&
    ctx !== null &&
    blocking.length === 0 &&
    !run?.running && !anotherRelease && announcementReady;

  function updateAnnouncement(patch: Partial<AnnouncementDraft>) {
    setAnnouncementDrafts((drafts) => ({ ...drafts, [draftKey]: { ...draft, ...patch } }));
  }

  function handleStart() {
    if (!project || !canStart || !ctx) return;
    setStarted(true);
    const announcement = versionLog ? {
      date: draft.date,
      list: announcementLines,
      logRevision: versionLog.revision,
    } : undefined;
    void startRelease(project, newVersion, ctx.releaseVersion, announcement);
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90dvh] overflow-auto sm:max-w-lg">
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
                    {key === "bump" && versionLog ? "同步版本和公告" : label}
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
                {versionLog ? "版本号和更新公告已同步。请按需提交发布变更。" : "package.json 版本号已更新。请按需提交发布变更。"}
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
                    版本号应以数字开头，可包含字母或 . - +
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

            {versionLog && (
              <div className="grid gap-3 rounded-md border p-3">
                <div>
                  <h3 className="text-sm font-medium">更新公告</h3>
                  <p className="break-all font-mono text-xs text-muted-foreground">{versionLog.path}</p>
                </div>
                {archivedVersion ? (
                  <p role="alert" className="text-xs text-destructive">此版本已有历史公告，请选择新版本。</p>
                ) : (
                  <>
                    <p className="text-xs text-muted-foreground">
                      {existing ? "已载入同版本公告，可以直接复用或修改。" : "构建前会追加本次公告。"}
                      {newVersion && " 版本 " + newVersion}
                    </p>
                    <div className="grid gap-1.5">
                      <Label htmlFor="release-announcement-date">发布日期</Label>
                      <Input id="release-announcement-date" type="date" value={draft.date} onChange={(event) => updateAnnouncement({ date: event.currentTarget.value })} />
                    </div>
                    <div className="grid gap-1.5">
                      <Label htmlFor="release-announcement-notes">更新内容</Label>
                      <textarea id="release-announcement-notes" rows={3} value={draft.notes}
                        onChange={(event) => updateAnnouncement({ notes: event.currentTarget.value })}
                        placeholder="每行一条，例如：优化报销审批日期筛选"
                        className="w-full rounded-lg border border-input bg-transparent px-2.5 py-1.5 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50" />
                    </div>
                  </>
                )}
              </div>
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
                  {blocking.length > 0 && (
                    <div className="rounded-md bg-destructive/10 p-2 text-xs text-destructive">
                      <p>工作区有未提交改动，请先提交或 stash 后再发布。</p>
                      <ul className="mt-1 list-disc pl-4 font-mono">
                        {blocking.slice(0, 8).map((file) => (
                          <li key={file} className="break-all">
                            {file}
                          </li>
                        ))}
                      </ul>
                      {blocking.length > 8 && (
                        <p className="mt-1">还有 {blocking.length - 8} 个</p>
                      )}
                    </div>
                  )}
                  {ctx.ignored.length > 0 && (
                    <p className="text-xs text-muted-foreground">
                      已忽略：{ctx.ignored.slice(0, 6).join("、")}
                      {ctx.ignored.length > 6 ? ` 等 ${ctx.ignored.length} 个` : ""}
                    </p>
                  )}
                  {ctx.allowBuildsChanges.length > 0 && (
                    <div className="space-y-1.5 rounded-md bg-muted p-2 text-xs text-muted-foreground">
                      <p>{ctx.allowBuildsIgnored ? "仅 allowBuilds 变化，预检允许继续。" : "pnpm-workspace.yaml 还有其他改动。"}</p>
                      <p>请审查并保留项目级规则，确认后可提交 pnpm-workspace.yaml。</p>
                      <ul className="max-h-24 space-y-0.5 overflow-auto font-mono">
                        {ctx.allowBuildsChanges.map((change) => (
                          <li key={change.package} className="break-all">
                            {change.package} · {change.before ?? "未设置"} → {change.after ?? "已移除"}
                          </li>
                        ))}
                      </ul>
                    </div>
                  )}
                </div>
              )
            )}

            <p className="text-xs text-muted-foreground">
              使用 Node {project?.release?.nodeVersion} 构建 {project?.release?.branch} 分支，
              将本次产物打包到 {project?.release?.outputDir || "dist"}。
            </p>
            {anotherRelease && <p className="text-xs text-muted-foreground">另一个项目正在发布，请等待完成后重新检查。</p>}

            <DialogFooter>
              {(blocking.length > 0 || ctxError) && (
                <Button variant="outline" onClick={loadContext}>
                  重新检查
                </Button>
              )}
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
