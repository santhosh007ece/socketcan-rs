use libc::{c_int, c_short, c_void, c_uint, socket, fcntl, close, connect, sockaddr, read, write,
           timeval, F_SETFL, O_NONBLOCK};

use futures;
use mio::{Evented, Ready, Poll, PollOpt, Token};
use mio::unix::EventedFd;
use nix::net::if_::if_nametoindex;
pub use nl::CanInterface;
use std::{io, slice, time};
use std::mem::size_of;
use std::io::{Error, ErrorKind};
use tokio_core::reactor::{Handle, PollEvented};

use {CanAddr, CanFrame, CanSocketOpenError, AF_CAN, EFF_FLAG, PF_CAN, SOCK_DGRAM, CAN_BCM,
     c_timeval_new};

pub const MAX_NFRAMES: u32 = 256;

/// OpCodes
///
/// create (cyclic) transmission task
pub const TX_SETUP: u32 = 1;
/// remove (cyclic) transmission task
pub const TX_DELETE: u32 = 2;
/// read properties of (cyclic) transmission task
pub const TX_READ: u32 = 3;
/// send one CAN frame
pub const TX_SEND: u32 = 4;
/// create RX content filter subscription
pub const RX_SETUP: u32 = 5;
/// remove RX content filter subscription
pub const RX_DELETE: u32 = 6;
/// read properties of RX content filter subscription
pub const RX_READ: u32 = 7;
/// reply to TX_READ request
pub const TX_STATUS: u32 = 8;
/// notification on performed transmissions (count=0)
pub const TX_EXPIRED: u32 = 9;
/// reply to RX_READ request
pub const RX_STATUS: u32 = 10;
/// cyclic message is absent
pub const RX_TIMEOUT: u32 = 11;
/// sent if the first or a revised CAN message was received
pub const RX_CHANGED: u32 = 12;

/// Flags
///
/// set the value of ival1, ival2 and count
pub const SETTIMER: u32 = 0x0001;
/// start the timer with the actual value of ival1, ival2 and count.
/// Starting the timer leads simultaneously to emit a can_frame.
pub const STARTTIMER: u32 = 0x0002;
/// create the message TX_EXPIRED when count expires
pub const TX_COUNTEVT: u32 = 0x0004;
/// A change of data by the process is emitted immediatly.
/// (Requirement of 'Changing Now' - BAES)
pub const TX_ANNOUNCE: u32 = 0x0008;
/// Copies the can_id from the message header to each subsequent frame
/// in frames. This is intended only as usage simplification.
pub const TX_CP_CAN_ID: u32 = 0x0010;
/// Filter by can_id alone, no frames required (nframes=0)
pub const RX_FILTER_ID: u32 = 0x0020;
/// A change of the DLC leads to an RX_CHANGED.
pub const RX_CHECK_DLC: u32 = 0x0040;
/// If the timer ival1 in the RX_SETUP has been set equal to zero, on receipt
/// of the CAN message the timer for the timeout monitoring is automatically
/// started. Setting this flag prevents the automatic start timer.
pub const RX_NO_AUTOTIMER: u32 = 0x0080;
/// refers also to the time-out supervision of the management RX_SETUP.
/// By setting this flag, when an RX-outs occours, a RX_CHANGED will be
/// generated when the (cyclic) receive restarts. This will happen even if the
/// user data have not changed.
pub const RX_ANNOUNCE_RESUM: u32 = 0x0100;
/// forces a reset of the index counter from the update to be sent by multiplex
/// message even if it would not be necessary because of the length.
pub const TX_RESET_MULTI_ID: u32 = 0x0200;
/// the filter passed is used as CAN message to be sent when receiving an RTR frame.
pub const RX_RTR_FRAME: u32 = 0x0400;
pub const CAN_FD_FRAME: u32 = 0x0800;

/// BcmMsgHead
///
/// Head of messages to and from the broadcast manager
#[repr(C)]
pub struct BcmMsgHead {
    _opcode: u32,
    _flags: u32,
    /// number of frames to send before changing interval
    _count: u32,
    /// interval for the first count frames
    _ival1: timeval,
    /// interval for the following frames
    _ival2: timeval,
    _can_id: u32,
    /// number of can frames appended to the message head
    _nframes: u32,
    // TODO figure out how why C adds a padding here?
    #[cfg(all(target_pointer_width = "32"))]
    _pad: u32,
    // TODO figure out how to allocate only nframes instead of MAX_NFRAMES
    /// buffer of CAN frames
    _frames: [CanFrame; MAX_NFRAMES as usize],
}

/// BcmMsgHeadFrameLess
///
/// Head of messages to and from the broadcast manager see _pad fields for differences
/// to BcmMsgHead
#[repr(C)]
pub struct BcmMsgHeadFrameLess {
    _opcode: u32,
    _flags: u32,
    /// number of frames to send before changing interval
    _count: u32,
    /// interval for the first count frames
    _ival1: timeval,
    /// interval for the following frames
    _ival2: timeval,
    _can_id: u32,
    /// number of can frames appended to the message head
    _nframes: u32,
    // Workaround Rust ZST has a size of 0 for frames, in
    // C the BcmMsgHead struct contains an Array that although it has
    // a length of zero still takes n (4) bytes.
    #[cfg(all(target_pointer_width = "32"))]
    _pad: usize,
}

#[repr(C)]
pub struct TxMsg {
    _msg_head: BcmMsgHeadFrameLess,
    _frames: [CanFrame; MAX_NFRAMES as usize],
}

impl BcmMsgHead {
    pub fn can_id(&self) -> u32 {
        self._can_id
    }

    #[inline]
    pub fn frames(&self) -> &[CanFrame] {
        return unsafe { slice::from_raw_parts(self._frames.as_ptr(), self._nframes as usize) };
    }
}

/// A socket for a CAN device, specifically for broadcast manager operations.
#[derive(Debug)]
pub struct CanBCMSocket {
    pub fd: c_int,
}

impl CanBCMSocket {
    /// Open a named CAN device non blocking.
    ///
    /// Usually the more common case, opens a socket can device by name, such
    /// as "vcan0" or "socan0".
    pub fn open_nb(ifname: &str) -> Result<CanBCMSocket, CanSocketOpenError> {
        let if_index = if_nametoindex(ifname)?;
        CanBCMSocket::open_if_nb(if_index)
    }

    /// Open CAN device by interface number non blocking.
    ///
    /// Opens a CAN device by kernel interface number.
    pub fn open_if_nb(if_index: c_uint) -> Result<CanBCMSocket, CanSocketOpenError> {

        // open socket
        let sock_fd;
        unsafe {
            sock_fd = socket(PF_CAN, SOCK_DGRAM, CAN_BCM);
        }

        if sock_fd == -1 {
            return Err(CanSocketOpenError::from(io::Error::last_os_error()));
        }

        let fcntl_resp = unsafe { fcntl(sock_fd, F_SETFL, O_NONBLOCK) };

        if fcntl_resp == -1 {
            return Err(CanSocketOpenError::from(io::Error::last_os_error()));
        }

        let addr = CanAddr {
            _af_can: AF_CAN as c_short,
            if_index: if_index as c_int,
            rx_id: 0, // ?
            tx_id: 0, // ?
        };

        let sockaddr_ptr = &addr as *const CanAddr;

        let connect_res;
        unsafe {
            connect_res = connect(
                sock_fd,
                sockaddr_ptr as *const sockaddr,
                size_of::<CanAddr>() as u32,
            );
        }

        if connect_res != 0 {
            return Err(CanSocketOpenError::from(io::Error::last_os_error()));
        }

        Ok(CanBCMSocket { fd: sock_fd })
    }

    fn close(&mut self) -> io::Result<()> {
        unsafe {
            let rv = close(self.fd);
            if rv != -1 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// Create a content filter subscription, filtering can frames by can_id.
    pub fn filter_id(
        &self,
        can_id: c_uint,
        ival1: time::Duration,
        ival2: time::Duration,
    ) -> io::Result<()> {
        let _ival1 = c_timeval_new(ival1);
        let _ival2 = c_timeval_new(ival2);

        let frames = [CanFrame::new(0x0, &[], false, false).unwrap(); MAX_NFRAMES as usize];
        let msg = BcmMsgHeadFrameLess {
            _opcode: RX_SETUP,
            _flags: SETTIMER | RX_FILTER_ID,
            _count: 0,
            #[cfg(all(target_pointer_width = "32"))]
            _pad: 0,
            _ival1: _ival1,
            _ival2: _ival2,
            _can_id: can_id | EFF_FLAG,
            _nframes: 0,
        };

        let tx_msg = &TxMsg {
            _msg_head: msg,
            _frames: frames,
        };

        let write_rv = unsafe {
            let tx_msg_ptr = tx_msg as *const TxMsg;
            write(self.fd, tx_msg_ptr as *const c_void, size_of::<TxMsg>())
        };

        if write_rv < 0 {
            return Err(Error::new(ErrorKind::WriteZero, io::Error::last_os_error()));
        }

        Ok(())
    }

    /// Remove a content filter subscription.
    pub fn filter_delete(&self, can_id: c_uint) -> io::Result<()> {
        let frames = [CanFrame::new(0x0, &[], false, false).unwrap(); MAX_NFRAMES as usize];

        let msg = &BcmMsgHead {
            _opcode: RX_DELETE,
            _flags: 0,
            _count: 0,
            _ival1: c_timeval_new(time::Duration::new(0, 0)),
            _ival2: c_timeval_new(time::Duration::new(0, 0)),
            _can_id: can_id,
            _nframes: 0,
            #[cfg(all(target_pointer_width = "32"))]
            _pad: 0,
            _frames: frames,
        };

        let write_rv = unsafe {
            let msg_ptr = msg as *const BcmMsgHead;
            write(self.fd, msg_ptr as *const c_void, size_of::<BcmMsgHead>())
        };

        let expected_size = size_of::<BcmMsgHead>() - size_of::<[CanFrame; MAX_NFRAMES as usize]>();
        if write_rv as usize != expected_size {
            let msg = format!("Wrote {} but expected {}", write_rv, expected_size);
            return Err(Error::new(ErrorKind::WriteZero, msg));
        }

        Ok(())
    }

    /// Read a single can frame.
    pub fn read_msg(&self) -> io::Result<BcmMsgHead> {

        let ival1 = c_timeval_new(time::Duration::from_millis(0));
        let ival2 = c_timeval_new(time::Duration::from_millis(0));
        let frames = [CanFrame::new(0x0, &[], false, false).unwrap(); MAX_NFRAMES as usize];
        let mut msg = BcmMsgHead {
            _opcode: 0,
            _flags: 0,
            _count: 0,
            _ival1: ival1,
            _ival2: ival2,
            _can_id: 0,
            _nframes: 0,
            #[cfg(all(target_pointer_width = "32"))]
            _pad: 0,
            _frames: frames,
        };

        let msg_ptr = &mut msg as *mut BcmMsgHead;
        let count = unsafe {
            read(
                self.fd.clone(),
                msg_ptr as *mut c_void,
                size_of::<BcmMsgHead>(),
            )
        };

        let last_error = io::Error::last_os_error();
        if count < 0 { Err(last_error) } else { Ok(msg) }
    }
}

impl Evented for CanBCMSocket {
    fn register(
        &self,
        poll: &Poll,
        token: Token,
        interest: Ready,
        opts: PollOpt,
    ) -> io::Result<()> {
        EventedFd(&self.fd).register(poll, token, interest, opts)
    }

    fn reregister(
        &self,
        poll: &Poll,
        token: Token,
        interest: Ready,
        opts: PollOpt,
    ) -> io::Result<()> {
        EventedFd(&self.fd).reregister(poll, token, interest, opts)
    }

    fn deregister(&self, poll: &Poll) -> io::Result<()> {
        EventedFd(&self.fd).deregister(poll)
    }
}

impl Drop for CanBCMSocket {
    fn drop(&mut self) {
        self.close().ok(); // ignore result
    }
}

pub struct BcmStream {
    io: PollEvented<CanBCMSocket>,
}

pub trait IntoBcmStream {
    type Stream: futures::stream::Stream;
    type Error;

    fn into_bcm(self) -> Result<Self::Stream, Self::Error>;
}

impl BcmStream {
    pub fn from(bcm_socket: CanBCMSocket, handle: &Handle) -> io::Result<BcmStream> {
        let io = try!(PollEvented::new(bcm_socket, handle));
        Ok(BcmStream { io: io })
    }
}

impl futures::stream::Stream for BcmStream {
    type Item = BcmMsgHead;
    type Error = io::Error;
    fn poll(&mut self) -> futures::Poll<Option<Self::Item>, Self::Error> {
        if let futures::Async::NotReady = self.io.poll_read() {
            return Ok(futures::Async::NotReady);
        }

        match self.io.get_ref().read_msg() {
            Ok(n) => Ok(futures::Async::Ready(Some(n))),
            Err(e) => {
                if e.kind() == io::ErrorKind::WouldBlock {
                    self.io.need_read();
                    return Ok(futures::Async::NotReady);
                }
                return Err(e);
            }
        }
    }
}