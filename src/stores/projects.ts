import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "sonner";
import type { Project } from "@/types";

/**
 * 项目列表 store（PRD 6.1 projectsStore）
 * 数据真身在 Rust 侧的 appDataDir/projects.json，前端持有镜像，
 * 每次变更后整表回写（项目数 < 20，整表写开销可忽略）。
 */
interface ProjectsState {
  projects: Project[];
  loaded: boolean;
  /** projectId -> 分支名（null = 非 Git 目录） */
  branches: Record<string, string | null>;

  load: () => Promise<void>;
  add: (project: Project) => Promise<void>;
  update: (project: Project) => Promise<void>;
  remove: (id: string) => Promise<void>;
  refreshBranches: () => Promise<void>;
}

export const useProjectsStore = create<ProjectsState>((set, get) => ({
  projects: [],
  loaded: false,
  branches: {},

  load: async () => {
    try {
      const projects = await invoke<Project[]>("load_projects");
      set({ projects, loaded: true });
      get().refreshBranches();
    } catch (e) {
      toast.error(`加载项目列表失败: ${e}`);
    }
  },

  add: async (project) => {
    await invoke("save_projects", { projects: [...get().projects, project] });
    set((s) => ({
      projects: [...s.projects, project],
      branches: { ...s.branches, [project.id]: null },
    }));
    get().refreshBranches();
  },

  update: async (project) => {
    await invoke("save_projects", {
      projects: get().projects.map((p) => (p.id === project.id ? project : p)),
    });
    set((s) => ({
      projects: s.projects.map((p) => (p.id === project.id ? project : p)),
    }));
    // 路径可能改了，重新读分支
    get().refreshBranches();
  },

  remove: async (id) => {
    await invoke("save_projects", {
      projects: get().projects.filter((p) => p.id !== id),
    });
    set((s) => {
      const branches = { ...s.branches };
      delete branches[id];
      return { projects: s.projects.filter((p) => p.id !== id), branches };
    });
  },

  refreshBranches: async () => {
    const { projects } = get();
    const entries = await Promise.all(
      projects.map(async (p) => {
        try {
          const branch = await invoke<string | null>("read_git_branch", {
            path: p.path,
          });
          return [p.id, branch] as const;
        } catch {
          return [p.id, null] as const;
        }
      }),
    );
    set({ branches: Object.fromEntries(entries) });
  },
}));
