import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { toast } from "sonner";

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Switch } from "@/components/ui/switch";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/** Rust AppSettings 的镜像 */
interface AppSettings {
  minimizeOnClose: boolean;
  autostart: boolean;
}

/** 设置弹窗：关闭行为 + 开机自启 + 版本号 */
export function SettingsDialog({ open, onOpenChange }: Props) {
  const [settings, setSettings] = useState<AppSettings>({
    minimizeOnClose: false,
    autostart: false,
  });
  const [version, setVersion] = useState("");

  useEffect(() => {
    if (!open) return;
    invoke<AppSettings>("load_settings")
      .then(setSettings)
      .catch((e) => toast.error(`读取设置失败: ${e}`));
    getVersion().then(setVersion).catch(() => setVersion("未知"));
  }, [open]);

  async function update(patch: Partial<AppSettings>) {
    const next = { ...settings, ...patch };
    setSettings(next); // 先改 UI，失败再回滚
    try {
      await invoke("save_settings", { settings: next });
    } catch (e) {
      setSettings(settings); // 回滚
      toast.error(`保存设置失败: ${e}`);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>设置</DialogTitle>
          <DialogDescription>修改后立即生效并自动保存。</DialogDescription>
        </DialogHeader>

        <div className="grid gap-6">
          <div className="grid gap-2">
            <Label>点击窗口关闭按钮（X）时</Label>
            <RadioGroup
              value={settings.minimizeOnClose ? "minimize" : "exit"}
              onValueChange={(v) =>
                update({ minimizeOnClose: v === "minimize" })
              }
              className="gap-2"
            >
              <label className="flex cursor-pointer items-center gap-2 rounded-md border p-3 has-[[data-state=checked]]:border-primary">
                <RadioGroupItem value="exit" id="close-exit" />
                <div>
                  <div className="text-sm font-medium">退出应用</div>
                  <div className="text-xs text-muted-foreground">
                    关闭时停止所有运行中的项目进程
                  </div>
                </div>
              </label>
              <label className="flex cursor-pointer items-center gap-2 rounded-md border p-3 has-[[data-state=checked]]:border-primary">
                <RadioGroupItem value="minimize" id="close-minimize" />
                <div>
                  <div className="text-sm font-medium">最小化到托盘</div>
                  <div className="text-xs text-muted-foreground">
                    项目继续运行，从托盘图标可重新打开
                  </div>
                </div>
              </label>
            </RadioGroup>
          </div>

          <div className="flex items-center justify-between rounded-md border p-3">
            <div>
              <div className="text-sm font-medium">开机自启动</div>
              <div className="text-xs text-muted-foreground">
                系统启动时自动运行本应用
              </div>
            </div>
            <Switch
              checked={settings.autostart}
              onCheckedChange={(v) => update({ autostart: v })}
              aria-label="开机自启动"
            />
          </div>

          <div className="text-center text-xs text-muted-foreground">
            版本 v{version || "…"}
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
