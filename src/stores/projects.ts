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
  addMany: (projects: Project[]) => Promise<void>;
  update: (project: Project) => Promise<void>;
  removeMany: (ids: string[]) => Promise<void>;
  refreshBranches: () => Promise<void>;
}

let pendingWrite = Promise.resolve();
let pendingLoad: Promise<void> | undefined;

function saveProjectList(change: (projects: Project[]) => Project[]): Promise<void> {
  const save = pendingWrite.then(async () => {
    const store = useProjectsStore.getState();
    if (!store.loaded) throw new Error("请先加载项目列表");
    const projects = change(store.projects);
    await invoke("save_projects", { projects });
    const currentBranches = useProjectsStore.getState().branches;
    const branches = Object.fromEntries(projects.map(({ id }) => [id, currentBranches[id] ?? null]));
    useProjectsStore.setState({ projects, branches });
  });
  pendingWrite = save.catch(() => {});
  return save;
}

export const useProjectsStore = create<ProjectsState>((set, get) => ({
  projects: [],
  loaded: false,
  branches: {},

  load: () => {
    if (pendingLoad) return pendingLoad;
    const load = pendingWrite.then(async () => {
      try {
        const projects = await invoke<Project[]>("load_projects");
        set({ projects, loaded: true });
        void get().refreshBranches();
      } catch (e) {
        toast.error(`加载项目列表失败: ${e}`);
      }
    });
    pendingWrite = load;
    pendingLoad = load.finally(() => { pendingLoad = undefined; });
    return pendingLoad;
  },

  add: (project) => get().addMany([project]),

  addMany: async (added) => {
    await saveProjectList((projects) => [...projects, ...added]);
    void get().refreshBranches();
  },

  update: async (project) => {
    await saveProjectList((projects) => projects.map((item) => item.id === project.id ? project : item));
    void get().refreshBranches();
  },

  removeMany: async (ids) => {
    const removed = new Set(ids);
    if (removed.size === 0) return;
    await saveProjectList((projects) => projects.filter(({ id }) => !removed.has(id)));
    void get().refreshBranches();
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
    if (get().projects === projects) set({ branches: Object.fromEntries(entries) });
  },
}));
