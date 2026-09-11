import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { useRuntimeStore } from "@/stores/runtime";

/**
 * 「未安装依赖，是否安装并启动？」确认弹窗（FR-11 依赖预检）。
 * start() 发现缺 node_modules 时把项目推入 pendingInstall，本组件消费：
 * 确认 → 逐个安装，安装成功后自动启动；跳过 → 什么都不做。
 */
export function InstallConfirmDialog() {
  const pendingInstall = useRuntimeStore((s) => s.pendingInstall);
  const resolvePendingInstall = useRuntimeStore((s) => s.resolvePendingInstall);
  const open = pendingInstall.length > 0;

  return (
    <AlertDialog
      open={open}
      onOpenChange={(o) => !o && resolvePendingInstall(false)}
    >
      <AlertDialogContent size="sm">
        <AlertDialogHeader>
          <AlertDialogTitle>
            {pendingInstall.length === 1
              ? `「${pendingInstall[0].name}」尚未安装依赖`
              : `${pendingInstall.length} 个项目尚未安装依赖`}
          </AlertDialogTitle>
          <AlertDialogDescription>
            以下项目缺少 node_modules，直接启动会报错：
            {pendingInstall.map((p) => `「${p.name}」`).join("、")}
            。确认后先安装依赖，安装完成将自动启动。
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel onClick={() => resolvePendingInstall(false)}>
            跳过
          </AlertDialogCancel>
          <AlertDialogAction onClick={() => resolvePendingInstall(true)}>
            安装并启动
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
