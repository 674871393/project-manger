import type { StartResult } from "@/stores/runtime";

export function describeStartResults(results: StartResult[]): string {
  const labels: Record<StartResult, string> = {
    started: "已启动进程",
    alreadyRunning: "已经在运行",
    needsDependencies: "待确认安装",
    failed: "启动失败",
  };
  return (Object.keys(labels) as StartResult[]).flatMap((key) => {
    const count = results.filter((result) => result === key).length;
    return count > 0 ? [labels[key] + " " + count + " 个"] : [];
  }).join("，");
}
