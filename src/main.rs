#[macro_use]
extern crate error_chain;

use std::ptr;

use log::debug;

use netif::{Runnable, HugeInfo};
use netif::server::ServerContext;

use nix::fcntl::{open, OFlag};
use nix::sys::mman::{mmap, munmap, MapFlags, ProtFlags};
use nix::sys::stat::Mode;

const LEN: usize = 2048 * 1024;

error_chain! {
    foreign_links {
        Io(::std::io::Error);
        Nix(::nix::Error);
    }

    links {
        NetIf(::netif::Error, ::netif::ErrorKind);
    }
}

fn main() -> Result<()> {
    pretty_env_logger::init();

    let path = "/dev/hugepages/dummy-server-00001";
    debug!("creating huge page at {}", path);

    let f = open(path, OFlag::O_CREAT | OFlag::O_RDWR, Mode::all())?;
    let base = unsafe { mmap(
        ptr::null_mut(),
        LEN,
        ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
        MapFlags::MAP_SHARED | MapFlags::MAP_HUGETLB,
        f,
        0
    )}?;
    let hp_info = vec![HugeInfo::new(base as *mut u8, LEN, f)];

    for h in &hp_info {;
        debug!("{}", h);
    }

    let mut ctx = ServerContext::new(hp_info)?;

    ctx.run()?;

    unsafe {
        munmap(base, LEN)
    }?;

    Ok(())
}
