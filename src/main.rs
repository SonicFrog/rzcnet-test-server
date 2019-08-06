#[macro_use]
extern crate error_chain;

use std::net::Ipv4Addr;
use std::ptr;
use std::thread;

use bounded_spsc_queue::{make, Consumer};

use log::{debug, info, error};

use netif::*;
use netif::server::*;

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

struct DummySched {
    command: Consumer<SchedulerClosure>,
}

impl DummySched {
    fn new(command: Consumer<SchedulerClosure>) -> Self {
        Self {
            command,
        }
    }
}

impl ServerScheduler for DummySched {
    fn register(&mut self, pid: u32, id: u32, addr: Ipv4Addr, port: u16, proto: IpProto) {
        info!("registering {}:{} as {} for client {}", addr, port, proto, pid);
    }

    fn add_in(&mut self, pid: u32, queue: ShmContainer<ShmPacket>) {
        info!("adding input queue {} for client {}", pid, queue);
    }

    fn add_out(&mut self, pid: u32, queue: ShmContainer<ShmPacket>) {
        info!("adding output queue {} for client {}", pid, queue);
    }
}

impl Runnable for DummySched {
    type Error = netif::Error;

    fn run(&mut self) -> std::result::Result<(), Self::Error> {
        loop {
            if let Some(closure) = self.command.try_pop() {
                info!("applying closure on scheduler");
                (closure)(self)
            }
        }
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

    for h in &hp_info {
        debug!("{}", h);
    }

    // creating the scheduler command channel
    let (prod, cons) = make(32);

    let producers = vec![prod];


    // spawning the dummy scheduler
    thread::spawn(move || {
        if let Err(e) = DummySched::new(cons).run() {
            error!("scheduler failed: {}", e);
        }
    });

    let mut ctx = ServerContext::new(hp_info, producers)?;

    ctx.run()?;

    unsafe {
        munmap(base, LEN)
    }?;

    Ok(())
}
