use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde::Serialize;

#[derive(Clone, Serialize)]
pub(crate) struct PortListener {
    pub(crate) port: u16,
    pub(crate) pid: u32,
    pub(crate) address: IpAddr,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PortOwner {
    #[serde(flatten)]
    listener: PortListener,
    process: Option<String>,
    project_id: Option<String>,
}

pub(crate) struct ProcessInfo {
    parent: u32,
    name: String,
}

pub(crate) fn owns_process(root: u32, mut pid: u32, processes: &HashMap<u32, ProcessInfo>) -> bool {
    for _ in 0..=processes.len() {
        if pid == root {
            return true;
        }
        let Some(info) = processes.get(&pid) else {
            return false;
        };
        pid = info.parent;
    }
    false
}

pub(crate) fn describe_listeners(roots: &[(String, u32)]) -> Result<Vec<PortOwner>, String> {
    let processes = process_snapshot()?;
    let mut listeners = tcp_listeners()?;
    listeners.sort_by_key(|entry| (entry.port, entry.pid, entry.address));
    Ok(listeners
        .into_iter()
        .map(|listener| {
            let project_id = roots
                .iter()
                .find(|(_, root)| owns_process(*root, listener.pid, &processes))
                .map(|(id, _)| id.clone());
            let process = processes.get(&listener.pid).map(|entry| entry.name.clone());
            PortOwner {
                listener,
                process,
                project_id,
            }
        })
        .collect())
}

#[cfg(windows)]
pub(crate) fn process_snapshot() -> Result<HashMap<u32, ProcessInfo>, String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    // The snapshot owns its handle. PROCESSENTRY32W requires dwSize to be initialized.
    unsafe {
        let handle = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if handle == INVALID_HANDLE_VALUE {
            return Err(format!(
                "读取进程列表失败: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut snapshot = HashMap::new();
        let mut found = Process32FirstW(handle, &mut entry);
        while found != 0 {
            let length = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            snapshot.insert(
                entry.th32ProcessID,
                ProcessInfo {
                    parent: entry.th32ParentProcessID,
                    name: String::from_utf16_lossy(&entry.szExeFile[..length]),
                },
            );
            found = Process32NextW(handle, &mut entry);
        }
        CloseHandle(handle);
        Ok(snapshot)
    }
}

#[cfg(windows)]
fn read_tcp_table<Row: Copy>(family: u32) -> Result<Vec<Row>, String> {
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, TCP_TABLE_OWNER_PID_LISTENER,
    };

    let mut size = 0u32;
    let mut buffer: Vec<u32> = Vec::new();
    for _ in 0..4 {
        let ptr = if buffer.is_empty() {
            std::ptr::null_mut()
        } else {
            buffer.as_mut_ptr().cast()
        };
        // Both owner-PID table formats have a u32 count followed by rows aligned to four bytes.
        let status = unsafe {
            GetExtendedTcpTable(ptr, &mut size, 0, family, TCP_TABLE_OWNER_PID_LISTENER, 0)
        };
        if status == ERROR_INSUFFICIENT_BUFFER {
            buffer.resize((size as usize).div_ceil(size_of::<u32>()), 0);
            continue;
        }
        if status != 0 {
            return Err(format!(
                "读取 TCP 监听列表失败: {}",
                std::io::Error::from_raw_os_error(status as i32)
            ));
        }
        let Some(&count) = buffer.first() else {
            return Ok(Vec::new());
        };
        let bytes = (buffer.len() - 1) * size_of::<u32>();
        if count as usize > bytes / size_of::<Row>() {
            return Err("TCP 监听列表长度无效".into());
        }
        let rows = buffer[1..].as_ptr().cast::<Row>();
        // Only the two C row types below are used. The byte count was verified above.
        return Ok((0..count as usize)
            .map(|i| unsafe { rows.add(i).read_unaligned() })
            .collect());
    }
    Err("TCP 监听列表变化过快，请重试".into())
}

#[cfg(windows)]
pub(crate) fn tcp_listeners() -> Result<Vec<PortListener>, String> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};

    let v4 = read_tcp_table::<MIB_TCPROW_OWNER_PID>(AF_INET as u32)?
        .into_iter()
        .map(|row| PortListener {
            port: u16::from_be(row.dwLocalPort as u16),
            pid: row.dwOwningPid,
            address: Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes()).into(),
        });
    let v6 = read_tcp_table::<MIB_TCP6ROW_OWNER_PID>(AF_INET6 as u32)?
        .into_iter()
        .map(|row| PortListener {
            port: u16::from_be(row.dwLocalPort as u16),
            pid: row.dwOwningPid,
            address: Ipv6Addr::from(row.ucLocalAddr).into(),
        });
    Ok(v4.chain(v6).collect())
}

#[cfg(not(windows))]
pub(crate) fn tcp_listeners() -> Result<Vec<PortListener>, String> {
    Err("监听端口查询仅支持 Windows".into())
}

#[cfg(not(windows))]
pub(crate) fn process_snapshot() -> Result<HashMap<u32, ProcessInfo>, String> {
    Err("进程归属查询仅支持 Windows".into())
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn finds_ipv4_listener_and_its_process() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let pid = std::process::id();
        let owners = super::describe_listeners(&[("self".into(), pid)]).unwrap();
        let owner = owners
            .iter()
            .find(|owner| owner.listener.port == port && owner.listener.pid == pid)
            .unwrap();
        assert_eq!(owner.project_id.as_deref(), Some("self"));
        assert_eq!(owner.listener.address.to_string(), "127.0.0.1");
    }
}
