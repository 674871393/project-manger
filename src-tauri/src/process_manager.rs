// 进程生命周期管理（PRD 6.3）
//
// Windows 上 `npm run dev` 是一棵进程树：cmd.exe → node (包管理器) → node (dev server)。
// 只 kill 最外层会留下孤儿 node 进程继续占着端口，因此：
// - spawn 与 kill 统一在本模块管理，Rust 侧持有 projectId -> pid 表；
// - 停止用 `taskkill /PID <pid> /T /F` 杀整棵树（不走「先优雅后强制」：控制台
//   node 进程不响应 WM_CLOSE，taskkill 不带 /F 只对 GUI 窗口有意义，对 dev
//   server 等于白等 3 秒）；
// - 应用退出时（RunEvent::ExitRequested，见 lib.rs）对所有运行中项目执行 stop_all；
// - stdout/stderr 按行转发 `log-line` 事件，进程退出发 `process-exit` 事件（含退出码）。

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::commands::Project;

/// CREATE_NO_WINDOW：spawn 和 taskkill 都要带，否则每个项目弹一个控制台黑窗
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Default)]
pub struct ProcessManager {
    /// projectId -> 根进程 pid（cmd.exe）；进程退出后由监控线程移除
    running: Mutex<HashMap<String, u32>>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct LogLineEvent {
    project_id: String,
    stream: &'static str,
    line: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ProcessExitEvent {
    project_id: String,
    code: Option<i32>,
}

impl ProcessManager {
    /// pid 登记表落盘（appDataDir/running-pids.json）。
    /// 应用崩溃时来不及 stop_all，下次启动凭这份文件清理孤儿进程（PRD 7 / FR P1）。
    fn persist_pids(&self, app: &AppHandle) {
        let Ok(dir) = app.path().app_data_dir() else {
            return;
        };
        let running = self.running.lock().unwrap().clone();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(json) = serde_json::to_string(&running) {
            let _ = std::fs::write(dir.join("running-pids.json"), json);
        }
    }

    /// 启动项目：`cmd /C "<pm> run <script>"`，成功返回根进程 pid。
    ///
    /// 经 cmd.exe 包一层是为了兼容 .cmd 脚本（npm.cmd 等，CreateProcess 无法直接执行）；
    /// 多出的 cmd.exe 一层由 taskkill /T 一并收掉，不构成孤儿风险。
    pub fn start(&self, app: &AppHandle, project: &Project) -> Result<u32, String> {
        // 锁内完成「查重 + spawn + 登记」，避免双击启动产生两个进程
        let mut child = {
            let mut running = self.running.lock().unwrap();
            if running.contains_key(&project.id) {
                return Err("项目已在运行".into());
            }
            let mut cmd = Command::new("cmd");
            cmd.args([
                "/C",
                &format!("{} run {}", project.package_manager, project.script),
            ])
            .current_dir(&project.path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
            if let Some(env) = &project.env {
                cmd.envs(env);
            }
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(CREATE_NO_WINDOW);
            }
            let child = cmd
                .spawn()
                .map_err(|e| format!("启动失败（检查路径/包管理器是否可用）: {e}"))?;
            running.insert(project.id.clone(), child.id());
            child
        };

        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // 两个读线程：stdout / stderr 各一个，按行转发事件
        // （擦成 Box<dyn Read> 才能放进同一个数组——两个管道类型不同）
        let pipes: [(&str, Option<Box<dyn std::io::Read + Send>>); 2] =
            [("stdout", stdout.map(|p| Box::new(p) as Box<dyn std::io::Read + Send>)),
             ("stderr", stderr.map(|p| Box::new(p) as Box<dyn std::io::Read + Send>))];
        for (stream, pipe) in pipes {
            if let Some(pipe) = pipe {
                let app_handle = app.clone();
                let project_id = project.id.clone();
                thread::spawn(move || {
                    for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                        let _ = app_handle.emit(
                            "log-line",
                            LogLineEvent {
                                project_id: project_id.clone(),
                                stream,
                                line,
                            },
                        );
                    }
                });
            }
        }

        // 监控线程：等进程退出，上报退出码并从 pid 表移除
        let app_handle = app.clone();
        let project_id = project.id.clone();
        thread::spawn(move || {
            let code = child.wait().ok().and_then(|s| s.code());
            let _ = app_handle.emit(
                "process-exit",
                ProcessExitEvent {
                    project_id: project_id.clone(),
                    code,
                },
            );
            if let Some(mgr) = app_handle.try_state::<ProcessManager>() {
                mgr.running.lock().unwrap().remove(&project_id);
                mgr.persist_pids(&app_handle);
            }
        });

        self.persist_pids(app);
        Ok(pid)
    }

    /// 停止项目（幂等：未在运行直接成功）。
    /// taskkill 报错但 pid 表里已无记录 = 进程恰好自己退出了，同样视为成功。
    pub fn stop(&self, project_id: &str) -> Result<(), String> {
        let Some(pid) = self.running.lock().unwrap().get(project_id).copied() else {
            return Ok(());
        };
        match kill_tree(pid) {
            Ok(()) => Ok(()),
            Err(e) => {
                if !self.running.lock().unwrap().contains_key(project_id) {
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }

    /// 应用退出前调用：杀掉全部进程树，失败只忽略（此时已无 UI 可报）
    pub fn stop_all(&self, app: &AppHandle) {
        let pids: Vec<u32> = self.running.lock().unwrap().values().copied().collect();
        for pid in pids {
            let _ = kill_tree(pid);
        }
        self.running.lock().unwrap().clear();
        self.persist_pids(app);
    }
}

/// 应用启动时清理上次崩溃残留的孤儿进程（PRD 7）。
/// 校验进程名只杀 node/cmd，防止 pid 被系统复用到无关进程上误杀。
pub fn cleanup_orphans(app: &AppHandle) {
    let Ok(dir) = app.path().app_data_dir() else {
        return;
    };
    let Ok(content) = std::fs::read_to_string(dir.join("running-pids.json")) else {
        return;
    };
    let Ok(pids) = serde_json::from_str::<HashMap<String, u32>>(&content) else {
        let _ = std::fs::remove_file(dir.join("running-pids.json"));
        return;
    };
    let mut cleaned = 0u32;
    for pid in pids.values() {
        if orphan_matches_dev_process(*pid) && kill_tree(*pid).is_ok() {
            cleaned += 1;
        }
    }
    let _ = std::fs::remove_file(dir.join("running-pids.json"));
    if cleaned > 0 {
        let _ = app.emit("orphans-cleaned", cleaned);
    }
}

/// pid 是否属于 node.exe / cmd.exe（tasklist CSV 首字段是映像名）
#[cfg(windows)]
fn orphan_matches_dev_process(pid: u32) -> bool {
    use std::os::windows::process::CommandExt;
    let Ok(output) = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return false;
    };
    let line = String::from_utf8_lossy(&output.stdout);
    let name = line.split(',').next().unwrap_or("").trim().trim_matches('"');
    matches!(name.to_ascii_lowercase().as_str(), "node.exe" | "cmd.exe")
}

#[cfg(not(windows))]
fn orphan_matches_dev_process(_pid: u32) -> bool {
    false
}

#[cfg(windows)]
fn kill_tree(pid: u32) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let output = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("taskkill 执行失败: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!("taskkill 失败: {msg}"))
    }
}

#[tauri::command]
pub fn start_project(
    app: AppHandle,
    state: State<ProcessManager>,
    project: Project,
) -> Result<u32, String> {
    state.start(&app, &project)
}

#[tauri::command]
pub fn stop_project(state: State<ProcessManager>, project_id: String) -> Result<(), String> {
    state.stop(&project_id)
}

/// 当前运行中的 projectId -> pid 表。
/// 前端页面重载（dev 热更新/刷新）后 zustand store 清空，但进程还活着，
/// 前端挂载时调这个命令找回真实状态。
#[tauri::command]
pub fn get_running(state: State<ProcessManager>) -> HashMap<String, u32> {
    state.running.lock().unwrap().clone()
}

/// 用 VSCode 打开项目目录（`code <path>`）。
/// code 是 .cmd 脚本，经 cmd /C 执行；spawn 后即返回不等退出。
#[tauri::command]
pub fn open_in_vscode(path: String) -> Result<(), String> {
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", "code", &path]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("打开 VSCode 失败（检查是否安装且在 PATH 中）: {e}"))
}

/// TCP 连通性探测（PRD FR-5）：前端 fetch 会受 CORS 限制，探 TCP 不受影响
#[tauri::command]
pub fn probe_port(port: u16, timeout_ms: u64) -> Result<(), String> {
    use std::net::TcpStream;
    use std::time::Duration;

    let addr = format!("127.0.0.1:{port}");
    let timeout = Duration::from_millis(timeout_ms);
    TcpStream::connect_timeout(
        &addr
            .parse()
            .map_err(|_| format!("无效地址: {addr}"))?,
        timeout,
    )
    .map(|_| ())
    .map_err(|_| format!("端口 {port} 未就绪"))
}

/// 本机局域网 IP（运行地址展示用，手机同 WiFi 调试等场景）。
///
/// UDP connect 公网地址的常见技巧在本机有 WSL/Docker 虚拟网卡时会选中虚拟段
/// （如 172.18.0.1），所以改为枚举全部本机地址后按优先级挑：
/// 192.168.* > 10.* > 172.16-31.*，排除回环与链路本地。拿不到时前端回退 localhost。
#[tauri::command]
pub fn get_lan_ip() -> Result<String, String> {
    use local_ip_address::list_afinet_netifas;

    let ifas = list_afinet_netifas().map_err(|e| format!("枚举网卡失败: {e}"))?;
    let ips: Vec<std::net::IpAddr> = ifas.into_iter().map(|(_, ip)| ip).collect();

    let is_private = |ip: std::net::IpAddr| matches!(ip, std::net::IpAddr::V4(v4) if !v4.is_loopback());

    let rank = |ip: std::net::IpAddr| match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            if o[0] == 192 && o[1] == 168 {
                0 // 常见家用/办公路由段，最可能是真实网卡
            } else if o[0] == 10 {
                1
            } else if (16..=31).contains(&o[0]) {
                2 // 常被 Docker/WSL 虚拟网卡占用
            } else {
                3
            }
        }
        std::net::IpAddr::V6(_) => 4,
    };

    ips.into_iter()
        .filter(|ip| is_private(*ip) && rank(*ip) < 3)
        .min_by_key(|ip| rank(*ip))
        .map(|ip| ip.to_string())
        .ok_or_else(|| "未找到局域网地址".into())
}
