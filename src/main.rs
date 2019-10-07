#[macro_use]
extern crate error_chain;

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::ptr;
use std::result::Result as StdRes;
use std::thread;
use std::time::Duration;

use bounded_spsc_queue::{make, Consumer};

use log::{debug, error, info, trace};

use netif::server::*;
use netif::Error as NetErr;
use netif::*;

use nix::fcntl::{open, OFlag};
use nix::sys::mman::{mmap, munmap, MapFlags, ProtFlags};
use nix::sys::stat::Mode;

/// Hugepage size
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

thread_local! {
    static SOCKETS: RefCell<HashMap<u32, Box<dyn Socket<Addr, ()>>>> = RefCell::new(HashMap::new());
    static CLIENTS: RefCell<HashMap<u32, QueuePair>> = RefCell::new(HashMap::new());
}

type Addr = (Ipv4Addr, u16);

type QueuePair = (ShmContainer<ShmPacket>, ShmContainer<ShmPacket>);

struct DummySocket {
    pid: u32,
    sid: u32,
    ip: Ipv4Addr,
    port: u16,
    proto: IpProto,
}

impl DummySocket {
    fn new(pid: u32, sid: u32, ip: Ipv4Addr, port: u16, proto: IpProto) -> Self {
        DummySocket {
            pid,
            sid,
            ip,
            port,
            proto,
        }
    }

    fn pid(&self) -> u32 {
        self.pid
    }
}

impl Shared for DummySocket {
    fn id(&self) -> u32 {
        self.sid
    }
}

impl PacketHandler<Addr, ()> for DummySocket {
    fn deliver(&self, _: &(), pkt: &[ShmPacket], addr: Addr) {
        info!(
            "delivered {} packets from {}:{} to {}",
            pkt.len(),
            addr.0,
            addr.1,
            self.id()
        );
    }
}

impl Socket<Addr, ()> for DummySocket {
    fn send_to(
        &self,
        pkts: &mut dyn Iterator<Item = OutPacket>,
        addr: Addr,
    ) -> StdRes<usize, NetErr> {
        info!("sent {} packets to {}:{}", pkts.count(), addr.0, addr.1);
        Ok(0)
    }

    fn addr(&self) -> Ipv4Addr {
        self.ip
    }

    fn port(&self) -> u16 {
        self.port
    }
}

struct DummySched {
    socket_in: Consumer<Box<dyn Socket<Addr, ()>>>,
    client_in: Consumer<(u32, QueuePair)>,
    packets: Consumer<ShmPacket>,
}

impl DummySched {
    fn new(
        socket_in: Consumer<Box<dyn Socket<Addr, ()>>>,
        client_in: Consumer<(u32, QueuePair)>,
        packets: Consumer<ShmPacket>,
    ) -> Self {
        Self {
            socket_in,
            client_in,
            packets,
        }
    }
}

impl Runnable for DummySched {
    type Error = netif::Error;

    fn run(&mut self) -> std::result::Result<(), Self::Error> {
        loop {
            while let Some((id, queues)) = self.client_in.try_pop() {
                info!("new client {}", id);
                CLIENTS.with(move |clients| clients.borrow_mut().insert(id, queues));
            }

            while let Some(socket) = self.socket_in.try_pop() {
                info!("scheduler received socket {}", socket.id());

                SOCKETS.with(move |sockets| {
                    sockets.borrow_mut().insert(socket.id(), socket);
                })
            }

            while let Some(pkt) = self.packets.try_pop() {
                trace!("fake delivering one packet to socket {}", pkt.id());
                SOCKETS.with(|sockets| {
                    if let Some(socket) = sockets.borrow().get(&pkt.id()) {
                        info!("found socket {}", socket.id());
                        match (socket as &dyn Any).downcast_ref::<DummySocket>() {
                            Some(concrete) => CLIENTS.with(|clients| {
                                if let Some((ref _input, ref mut output)) =
                                    clients.borrow_mut().get_mut(&concrete.pid)
                                {
                                    output.push_all(&[pkt]).expect("failed to push packets");
                                } else {
                                    error!("unknown client PID {}", concrete.pid);
                                }
                            }),
                            None => error!("bad socket type!"),
                        }
                    } else {
                        error!("unknown socket {}", pkt.id())
                    }
                })
            }
        }
    }
}

fn main() -> Result<()> {
    pretty_env_logger::init();

    let path = "/dev/hugepages/dummy-server-00001";
    debug!("creating huge page at {}", path);

    let f = open(path, OFlag::O_CREAT | OFlag::O_RDWR, Mode::all())?;
    let base = unsafe {
        mmap(
            ptr::null_mut(),
            LEN,
            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
            MapFlags::MAP_SHARED | MapFlags::MAP_HUGETLB,
            f,
            0,
        )
    }?;
    let hp_info = vec![HugeInfo::new(base as *mut u8, LEN, f)];

    for h in &hp_info {
        debug!("{}", h);
    }

    let (pkt_prod, pkt_cons) = make(32);
    let (sock_prod, sock_cons) = make(32);
    let (client_prod, client_cons) = make(32);

    // spawning the dummy scheduler
    thread::spawn(move || {
        if let Err(e) = DummySched::new(sock_cons, client_cons, pkt_cons).run() {
            error!("scheduler failed: {}", e);
        }
    });

    let client_cb = Box::new(move |pid, idx, pair| {
        info!(
            "new client queue registered for PID {} with index {}",
            pid, idx
        );

        client_prod.push((pid, pair));

        0i32
    });
    let socket_cb = Box::new(move |pid, sid, ip: u32, port, proto: u8| {
        info!(
            "new socket for client {} with id {} and destination {}:{}",
            pid,
            sid,
            Ipv4Addr::from(ip),
            port
        );

        sock_prod.push(Box::new(DummySocket::new(
            pid,
            sid,
            ip.into(),
            port,
            proto.into(),
        )));

        0
    });
    let mut ctx = ServerContext::new(hp_info, client_cb, socket_cb)?;

    let _handle = thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(1));
        info!("delivering one dummy packet");

        if pkt_prod.try_push(ShmPacket::default()).is_some() {
            error!("failed to push packet");
        }
    });
    ctx.run()?;
    unsafe { munmap(base, LEN) }?;

    Ok(())
}
