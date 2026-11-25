// 数据模型（PRD 第 5 节）

export type PackageManager = "npm" | "pnpm" | "yarn" | "bun";
export type Framework = "vite" | "next" | "nuxt" | "webpack" | "other";

/** 一键发布配置（PRD FR-20，可选启用） */
export interface ReleaseConfig {
  /** 发布分支 */
  branch: string;
  /** nvm 完整版本号，如 "16.20.0" */
  nodeVersion: string;
  /** 构建脚本名，如 "build:prod" */
  script: string;
  /** 相对项目目录，旧配置默认 dist */
  outputDir?: string;
  /**
   * 预检额外忽略的路径（文件名、相对路径或简单 glob）。
   * `.env.development` 始终忽略，不必写在这里。
   */
  ignorePaths?: string[];
}

/** read_release_context 的返回（FR-20 发布弹窗：版本基准 + 工作区状态） */
export interface ReleaseContext {
  /** 发布基准：发布分支上已提交的 package.json 版本 */
  releaseVersion: string;
  /** 实际取到基准的引用（本地分支名，或回退的 origin/<分支>） */
  releaseRef: string;
  /** 当前工作区分支（detached HEAD 时为短哈希） */
  currentBranch: string | null;
  /** 当前工作区 package.json 的版本（可能与发布基准不同） */
  workingVersion: string | null;
  /** 是否还有会拦住发布的未提交变更（已忽略的文件不算） */
  dirty: boolean;
  /** 会拦住发布的路径 */
  blocking: string[];
  /** 预检已忽略的路径（.env.development、配置项、仅 allowBuilds 变化的 pnpm-workspace.yaml） */
  ignored: string[];
  /** pnpm-workspace.yaml 的差异只有 allowBuilds 块 */
  allowBuildsIgnored: boolean;
  allowBuildsChanges: { package: string; before: string | null; after: string | null }[];
}

export interface ReleaseRecord {
  id: string;
  projectId: string;
  projectName: string;
  version: string;
  branch: string;
  commit: string | null;
  nodeVersion: string;
  startedAt: number;
  durationMs: number;
  success: boolean;
  zipPath: string | null;
  error: string | null;
}

export interface LaunchPlan {
  id: string;
  name: string;
  projectIds: string[];
}

export interface LaunchPlanConfig {
  plans: LaunchPlan[];
  lastUsedId: string | null;
}

type VersionCheck = { Ok: string } | { Err: string };

export interface ProjectDiagnostics {
  directoryExists: boolean;
  packageExists: boolean;
  workspaceRoot: string | null;
  detectedPackageManager: string | null;
  node: VersionCheck;
  packageManager: VersionCheck;
  dependenciesAvailable: boolean;
  scriptExists: boolean;
  gitBranch: string | null;
}

export interface PortOwner {
  port: number;
  pid: number;
  address: string;
  process: string | null;
  projectId: string | null;
}

/** 持久化到 appDataDir/projects.json */
export interface Project {
  id: string;
  name: string;
  path: string;
  packageManager: PackageManager;
  script: string;
  manualPort?: number;
  env?: Record<string, string>;
  framework?: Framework;
  release?: ReleaseConfig;
  createdAt: number;
}

/** inspect_project 命令的返回：添加项目时的自动识别结果 */
export interface ProjectInspection {
  name: string;
  packageManager: PackageManager;
  script: string;
  framework: Framework;
}

/** 运行时状态（内存，不持久化；M2 进程核心时使用） */
export type RunStatus = "stopped" | "starting" | "running" | "ready" | "error";

export interface RuntimeState {
  projectId: string;
  status: RunStatus;
  pid?: number;
  url?: string;
  startedAt?: number;
  exitCode?: number;
}
