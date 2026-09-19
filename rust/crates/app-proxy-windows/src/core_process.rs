//! Exact core process ownership and native TCP listener evidence. Dropping a
//! handle preserves the shared core; only explicit stop terminates it.
use crate::{
    Error, Result, core_state::CoreGeneration, identity, last_error, singbox_binary::CoreBinary,
};
use app_proxy_core::{ProcessIdentity, model::Endpoint};
use std::{
    mem::{offset_of, size_of},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    os::windows::{
        io::{AsRawHandle, OwnedHandle},
        process::CommandExt,
    },
    process::{Command, Stdio},
};
use windows_sys::Win32::{
    Foundation::*,
    NetworkManagement::IpHelper::*,
    Networking::WinSock::{AF_INET, AF_INET6},
    System::Threading::*,
};

pub struct CoreProcess {
    handle: OwnedHandle,
    identity: ProcessIdentity,
}
impl CoreProcess {
    pub fn identity(&self) -> &ProcessIdentity {
        &self.identity
    }

    pub fn spawn(binary: &CoreBinary, generation: &CoreGeneration) -> Result<Self> {
        identity::assert_ordinary_user()?;
        binary.verify_current()?;
        let caller = identity::current()?;
        let expected_image = identity::file_identity(binary.executable())?;
        let mut child = Command::new(binary.executable())
            .args(["run", "-c"])
            .arg(generation.config_path())
            .current_dir(
                generation
                    .config_path()
                    .parent()
                    .expect("absolute generation"),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
        // SAFETY: child retains the exact original process handle during inspection.
        let inspected = unsafe { identity::inspect_handle(child.as_raw_handle()) };
        let inspected = match inspected {
            Ok(p)
                if p.user_sid == caller.user_sid
                    && p.session_id == caller.session_id
                    && p.image_file == expected_image =>
            {
                p
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Invalid("CORE_PROCESS_IDENTITY_UNCONFIRMED"));
            }
        };
        let handle: OwnedHandle = child.into();
        Ok(Self {
            handle,
            identity: inspected,
        })
    }

    /// Only a previously persisted complete identity may be reattached. Never
    /// discovers by process name, image path alone, or an open proxy port.
    pub fn attach(expected: &ProcessIdentity) -> Result<Self> {
        identity::assert_ordinary_user()?;
        let caller = identity::current()?;
        if expected.user_sid != caller.user_sid || expected.session_id != caller.session_id {
            return Err(Error::IdentityMismatch);
        }
        let handle = identity::open(
            expected.pid,
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
        )?;
        // SAFETY: owned handle stays live across identity validation and later use.
        if unsafe { identity::inspect_handle(handle.as_raw_handle())? } != *expected {
            return Err(Error::IdentityMismatch);
        }
        Ok(Self {
            handle,
            identity: expected.clone(),
        })
    }

    /// None only when the original process is confirmed gone. Access denied or
    /// changed identity details are unknown; they never authorize a fresh spawn.
    pub fn recover(expected: &ProcessIdentity) -> Result<Option<Self>> {
        identity::assert_ordinary_user()?;
        let caller = identity::current()?;
        if expected.user_sid != caller.user_sid || expected.session_id != caller.session_id {
            return Err(Error::IdentityMismatch);
        }
        let handle = match identity::open(
            expected.pid,
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
        ) {
            Ok(handle) => handle,
            Err(Error::Windows {
                code: ERROR_INVALID_PARAMETER,
                ..
            }) => return Ok(None),
            Err(error) => return Err(error),
        };
        // SAFETY: retained handle is live; exit check precedes image-file lookup,
        // which can legitimately fail after an exited core has been uninstalled.
        unsafe {
            match WaitForSingleObject(handle.as_raw_handle(), 0) {
                WAIT_OBJECT_0 => return Ok(None),
                WAIT_TIMEOUT => {}
                _ => return Err(last_error("RecoverCoreWait")),
            }
            let actual = identity::inspect_handle(handle.as_raw_handle())?;
            if actual.creation_time != expected.creation_time {
                return Ok(None);
            }
            if actual != *expected {
                return Err(Error::IdentityMismatch);
            }
        }
        Ok(Some(Self {
            handle,
            identity: expected.clone(),
        }))
    }

    pub fn is_running(&self) -> Result<bool> {
        // SAFETY: live retained process handle and a zero-duration wait.
        match unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) } {
            WAIT_OBJECT_0 => Ok(false),
            WAIT_TIMEOUT => Ok(true),
            _ => Err(last_error("CoreProcessWait")),
        }
    }

    pub fn listeners_verified(&self, endpoints: &[Endpoint]) -> Result<bool> {
        if !self.is_running()? {
            return Ok(false);
        }
        // SAFETY: retained handle prevents PID reuse throughout the table snapshot.
        if unsafe { identity::inspect_handle(self.handle.as_raw_handle())? } != self.identity {
            return Err(Error::IdentityMismatch);
        }
        if endpoints.is_empty()
            || endpoints
                .iter()
                .any(|e| !e.host.is_loopback() || e.port == 0)
        {
            return Err(Error::Invalid("INVALID_CORE_ENDPOINTS"));
        }
        let mut rows = Vec::new();
        if endpoints.iter().any(|e| e.host.is_ipv4()) {
            rows.extend(listeners(AF_INET as u32)?);
        }
        if endpoints.iter().any(|e| e.host.is_ipv6()) {
            rows.extend(listeners(AF_INET6 as u32)?);
        }
        let verified = endpoints.iter().all(|endpoint| {
            let matching: Vec<_> = rows
                .iter()
                .filter(|r| r.address == endpoint.host && r.port == endpoint.port)
                .collect();
            !matching.is_empty() && matching.iter().all(|r| r.pid == self.identity.pid)
        });
        Ok(verified && self.is_running()?)
    }

    pub fn stop(&self) -> Result<()> {
        if !self.is_running()? {
            return Ok(());
        }
        // SAFETY: identity is checked using the same retained handle that is stopped.
        unsafe {
            if identity::inspect_handle(self.handle.as_raw_handle())? != self.identity {
                return Err(Error::IdentityMismatch);
            }
            if TerminateProcess(self.handle.as_raw_handle(), 0) == 0 {
                return Err(last_error("StopCoreProcess"));
            }
            if WaitForSingleObject(self.handle.as_raw_handle(), 3000) != WAIT_OBJECT_0 {
                return Err(Error::Invalid("CORE_STOP_UNCONFIRMED"));
            }
        }
        Ok(())
    }
}

struct Listener {
    address: IpAddr,
    port: u16,
    pid: u32,
}
fn listeners(family: u32) -> Result<Vec<Listener>> {
    let mut bytes = 0u32;
    // SAFETY: first call obtains the required size; null table is allowed.
    let code = unsafe {
        GetExtendedTcpTable(
            std::ptr::null_mut(),
            &mut bytes,
            0,
            family,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        )
    };
    if code != ERROR_INSUFFICIENT_BUFFER && code != 0 {
        return Err(Error::Windows {
            operation: "CoreTcpTableSize",
            code,
        });
    }
    for _ in 0..4 {
        if !(4..=16 * 1024 * 1024).contains(&bytes) {
            return Err(Error::Invalid("CORE_TCP_TABLE_SIZE"));
        }
        let capacity = bytes;
        let mut buffer = vec![0u64; (bytes as usize).div_ceil(8)];
        // SAFETY: u64 storage has adequate alignment and capacity for either table.
        let code = unsafe {
            GetExtendedTcpTable(
                buffer.as_mut_ptr().cast(),
                &mut bytes,
                0,
                family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        if code == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if code != 0 {
            return Err(Error::Windows {
                operation: "CoreTcpTable",
                code,
            });
        }
        if bytes > capacity || bytes < 4 {
            return Err(Error::Invalid("CORE_TCP_TABLE_SIZE"));
        }
        // SAFETY: size checked, first DWORD is the initialized entry count.
        let count = unsafe { *buffer.as_ptr().cast::<u32>() } as usize;
        let (offset, stride) = if family == AF_INET as u32 {
            (
                offset_of!(MIB_TCPTABLE_OWNER_PID, table),
                size_of::<MIB_TCPROW_OWNER_PID>(),
            )
        } else {
            (
                offset_of!(MIB_TCP6TABLE_OWNER_PID, table),
                size_of::<MIB_TCP6ROW_OWNER_PID>(),
            )
        };
        if (bytes as usize) < offset || count > (bytes as usize - offset) / stride {
            return Err(Error::Invalid("CORE_TCP_TABLE_SIZE"));
        }
        let mut result = Vec::with_capacity(count);
        for i in 0..count {
            // SAFETY: row boundaries validated above; C rows contain only integers.
            unsafe {
                let pointer = buffer.as_ptr().cast::<u8>().add(offset + i * stride);
                if family == AF_INET as u32 {
                    let row = pointer.cast::<MIB_TCPROW_OWNER_PID>().read_unaligned();
                    result.push(Listener {
                        address: Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes()).into(),
                        port: u16::from_be(row.dwLocalPort as u16),
                        pid: row.dwOwningPid,
                    });
                } else {
                    let row = pointer.cast::<MIB_TCP6ROW_OWNER_PID>().read_unaligned();
                    result.push(Listener {
                        address: Ipv6Addr::from(row.ucLocalAddr).into(),
                        port: u16::from_be(row.dwLocalPort as u16),
                        pid: row.dwOwningPid,
                    });
                }
            }
        }
        return Ok(result);
    }
    Err(Error::Invalid("CORE_TCP_TABLE_CHANGED"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_tables_verify_ipv4_ipv6_and_full_process_identity() {
        let own = identity::current().unwrap();
        let retained = CoreProcess::attach(&own).unwrap();
        for address in ["127.0.0.1:0", "[::1]:0"] {
            let listener = std::net::TcpListener::bind(address).unwrap();
            let local = listener.local_addr().unwrap();
            let endpoint = Endpoint {
                host: local.ip(),
                port: local.port(),
            };
            assert!(
                retained
                    .listeners_verified(std::slice::from_ref(&endpoint))
                    .unwrap()
            );
            drop(listener);
            assert!(!retained.listeners_verified(&[endpoint]).unwrap());
        }
        let mut forged = own;
        forged.creation_time += 1;
        assert!(matches!(
            CoreProcess::attach(&forged),
            Err(Error::IdentityMismatch)
        ));
    }
}
