use std::io;

use tokio::process::Command;

pub(crate) fn configure(command: &mut Command) -> io::Result<()> {
    // Reject alternate syscall ABIs instead of allowing a compatibility-mode bypass.
    let architecture = if cfg!(target_arch = "x86_64") {
        0xc000_003e
    } else if cfg!(target_arch = "aarch64") {
        0xc000_00b7
    } else {
        return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
    };
    let mut filter = vec![
        instruction(0x20, 0, 0, 4),
        instruction(0x15, 1, 0, architecture),
        instruction(0x06, 0, 0, 0x8000_0000),
        instruction(0x20, 0, 0, 0),
        instruction(0x35, 0, 1, 0x4000_0000),
        instruction(0x06, 0, 0, 0x8000_0000),
    ];
    for syscall in [
        libc::SYS_socket,
        libc::SYS_socketpair,
        libc::SYS_connect,
        libc::SYS_bind,
        libc::SYS_listen,
        libc::SYS_accept,
        libc::SYS_accept4,
        libc::SYS_sendto,
        libc::SYS_sendmsg,
        libc::SYS_recvfrom,
        libc::SYS_recvmsg,
        libc::SYS_io_uring_setup,
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
    ] {
        filter.push(instruction(0x15, 0, 1, syscall as u32));
        filter.push(instruction(0x06, 0, 0, 0x0005_0000 | libc::EPERM as u32));
    }
    filter.push(instruction(0x06, 0, 0, 0x7fff_0000));
    // The filter is allocated in the parent; pre_exec must not allocate after fork.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            let program = libc::sock_fprog {
                len: filter.len() as u16,
                filter: filter.as_ptr() as *mut libc::sock_filter,
            };
            if libc::prctl(libc::PR_SET_SECCOMP, 2, &program, 0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

fn instruction(code: u16, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}
