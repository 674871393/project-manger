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
export type RunStatus = "stopped" | "starting" | "running" | "error";

export interface RuntimeState {
  projectId: string;
  status: RunStatus;
  pid?: number;
  url?: string;
  startedAt?: number;
  exitCode?: number;
}
