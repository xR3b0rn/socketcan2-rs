//! SocketCAN support.
//! # Features
//! * Receive can frames
//! * Accurate timestamps (timestamps also support multi threading in contrast to receiving the TIMESTAMP via an ioctl call, which does not support mt)
//! * epoll-support (what allows to wait on multiple CAN devices in the same thread)
//! * Send CAN frames (not implemented yet)
//! * Filter CAN frames (not implemented yet)
//! # Usage example
//! ```
//! #[cfg(test)]
//! fn on_recv(msg: &Box<Msg>, _user_data: &u64) {
//!   println!("timestamp: {:?}", msg.timestamp());
//!   print!("received CAN frame (id: {}): ", msg.can_id());
//!   for i in 0..msg.len() {
//!     print!("{} ", msg[i as usize]);
//!   }
//!   println!("");
//! }
//! 
//! #[cfg(test)]
//! mod tests {
//!   use super::*;
//!   #[test]
//!   fn it_works() {
//!     let can = Can::open("vcan0").unwrap();
//!     let mut cg = CanGroup::<u64>::new();
//!     cg.add(can, 0).unwrap();
//!     match cg.next(Duration::milliseconds(-1), on_recv) {
//!       Ok(no_timeout) => if !no_timeout { panic!("timeout"); },
//!       Err(_) => panic!("error"),
//!     }
//!   }
//! }
//! ```

use std::mem;
use std::io;
use std::ops::Index;
use std::{os::raw::{c_char, c_int, c_void}};

use chrono::Duration;

#[allow(invalid_value)]

// Constants stolen from C headers
const AF_CAN: c_int = 29;
const PF_CAN: c_int = 29;
// Unused yet
// const CAN_RAW: c_int = 1;
// const SOL_CAN_BASE: c_int = 100;
// const SOL_CAN_RAW: c_int = SOL_CAN_BASE + CAN_RAW;
// const CAN_RAW_FILTER: c_int = 1;
// const CAN_RAW_ERR_FILTER: c_int = 2;
// const CAN_RAW_LOOPBACK: c_int = 3;
// const CAN_RAW_RECV_OWN_MSGS: c_int = 4;
// const CAN_RAW_FD_FRAMES: c_int = 5;
// const CAN_RAW_JOIN_FILTERS: c_int = 6;
// const SIOCGSTAMP: c_int = 0x8906;
// const SIOCGSTAMPNS: c_int = 0x8907;
/// if set, indicate 29 bit extended format
pub const EFF_FLAG: u32 = 0x80000000;
/// remote transmission request flag
pub const RTR_FLAG: u32 = 0x40000000;
/// error flag
pub const ERR_FLAG: u32 = 0x20000000;
/// valid bits in standard frame id
pub const SFF_MASK: u32 = 0x000007ff;
/// valid bits in extended frame id
pub const EFF_MASK: u32 = 0x1fffffff;
/// valid bits in error frame
pub const ERR_MASK: u32 = 0x1fffffff;
/// an error mask that will cause SocketCAN to report all errors
pub const ERR_MASK_ALL: u32 = ERR_MASK;
/// an error mask that will cause SocketCAN to silently drop all errors
pub const ERR_MASK_NONE: u32 = 0;

/// CAN socket
///
/// Provides standard functionallity for sending and receiving CAN frames.
pub struct Can {
  fd: c_int,
}
impl Can
{
  /// Open CAN device by netdev name.
  pub fn open(ifname: &str) -> Result<Can, io::Error> {
    unsafe {
      if ifname.len() > 16 {
        return Err(io::Error::new(io::ErrorKind::Other, "No such device"));
      }
      let fd = libc::socket(PF_CAN, libc::SOCK_RAW, libc::CAN_RAW);
      let mut uaddr = mem::MaybeUninit::<libc::sockaddr_can>::uninit();
      let mut addr = uaddr.as_mut_ptr();
      (*addr).can_family = AF_CAN as u16;
      let mut cifname = [0 as c_char; 17];
      for (i, ch) in ifname.chars().enumerate() {
        cifname[i] = ch as i8;
      }
      (*addr).can_ifindex = libc::if_nametoindex(&cifname as *const c_char) as i32;
      if (*addr).can_ifindex == 0 {
        return Err(io::Error::last_os_error());
      }
      let can = Can { fd: fd };
      {
        let timestamp_on: c_int = 1;
        if libc::setsockopt(can.fd, libc::SOL_SOCKET, libc::SO_TIMESTAMP, &timestamp_on as *const c_int as *const c_void, mem::size_of::<c_int>() as u32 + 2) < 0 {
          return Err(io::Error::last_os_error());
        }
      }
      {
        let opt_on: c_int = 1;
        libc::setsockopt(can.fd, libc::SOL_CAN_RAW, libc::CAN_RAW_FD_FRAMES, &opt_on as *const c_int as *const c_void, mem::size_of::<c_int>() as u32);
      }
      if libc::bind(can.fd, addr as *const libc::sockaddr_can as *const libc::sockaddr, mem::size_of::<libc::sockaddr_can>() as u32) != 0 {
        return Err(io::Error::last_os_error());
      }
      Ok(can)
    }
  }
  /// Receives a CAN message.
  /// Blocks until frame is received or the iface is down.
  pub fn recv(&self, msg: &mut Msg) -> Result<(), io::Error> {
    unsafe {
      msg.reset();
      let nbytes = libc::recvmsg(self.fd, mem::transmute(&msg.msg), 0);
      if nbytes < 0 {
        return Err(io::Error::last_os_error());
      }
    }
    Ok(())
  }
}
impl Drop for Can {
  fn drop(&mut self) {
    unsafe {
      if self.fd != 0 {
        libc::close(self.fd);
      }
      self.fd = 0;
    }
  }
}
struct CanData<T> {
  can: Can, 
  user_data: T,
}
/// CAN message type.
pub struct Msg {
  msg: libc::msghdr,
  addr: libc::sockaddr_can,
  iov: libc::iovec,
  frame: libc::canfd_frame,
  ctrlmsg: [u8; unsafe { libc::CMSG_SPACE(mem::size_of::<libc::timeval>() as u32) + 
                         libc::CMSG_SPACE(3 * mem::size_of::<libc::timespec>() as u32) +
                         libc::CMSG_SPACE(mem::size_of::<u32>() as u32) } as usize],
}
impl Msg {
  /// Return initialized empty message object.
  /// The return type is Box, to avoid dangling pointers
  /// when the object is moved.
  pub fn new() -> Box<Msg> {
    unsafe {
      let ucm             = mem::MaybeUninit::<Msg>::uninit();
      let mut msg         = Box::<Msg>::new(mem::transmute(ucm));
      msg.msg.msg_iovlen  = 1;
      msg.iov.iov_base    = mem::transmute(&(*msg).frame);
      msg.msg.msg_name    = mem::transmute(&(*msg).addr);
      msg.msg.msg_iov     = mem::transmute(&(*msg).iov);
      msg.msg.msg_control = mem::transmute(&(*msg).ctrlmsg[0]);
      msg.reset();
      msg
    }
  }
  fn reset(&mut self) {
    self.msg.msg_iovlen     = 1;
    self.iov.iov_len        = mem::size_of::<libc::canfd_frame>();
    self.msg.msg_namelen    = mem::size_of::<libc::sockaddr_can>() as u32;
    self.msg.msg_controllen = mem::size_of_val(&self.ctrlmsg);
    self.msg.msg_flags      = 0;
  }
  /// Get CAN ID.
  pub fn can_id(&self) -> u32 {
    self.frame.can_id
  }
  /// Get DLC.
  pub fn len(&self) -> u8 {
    self.frame.len
  }
  /// Get CAN FD flags.
  pub fn flags(&self) -> u8 {
    self.frame.flags
  }
  /// Get the frame timestamp.
  pub fn timestamp(&self) -> io::Result<Duration> {
    unsafe {
      let mut cmsg = libc::CMSG_FIRSTHDR(&self.msg);
      loop {
        if cmsg.is_null() || (*cmsg).cmsg_level != libc::SOL_SOCKET {
          break
        }
        match (*cmsg).cmsg_type {
          libc::SO_TIMESTAMP => {
            let tv = libc::CMSG_DATA(cmsg) as *const libc::timeval;            
            return Ok(Duration::milliseconds(((*tv).tv_sec * 1000000000 + (*tv).tv_usec * 1000) as i64));
          }
          libc::SO_TIMESTAMPING => {
            let ts = libc::CMSG_DATA(cmsg) as *const libc::timespec; 
            return Ok(Duration::milliseconds(((*ts).tv_sec * 1000000000 + (*ts).tv_nsec) as i64));
          }
          _ => {
          }
        };
        cmsg = libc::CMSG_NXTHDR(&self.msg, cmsg);
      }
    }
    Err(io::Error::new(io::ErrorKind::Unsupported, "timestamps aren't supported"))
  }
}
impl Index<usize> for Msg {
  type Output = u8;
  fn index(&self, index: usize) -> &u8 {
    &self.frame.data[index]
  }
}
/// Type for receiving data from multiple CAN devices. This type also supports timeouts.
pub struct CanGroup<T> {
  fd_epoll: c_int,
  cans: Vec<CanData<T>>,
  events: Vec<libc::epoll_event>,
  msg: Box<Msg>,
}
impl<T> CanGroup<T> {
  /// Creates an empty CanGroup instance.
  pub fn new() -> CanGroup<T> {
    unsafe {
      CanGroup::<T> {
        fd_epoll: libc::epoll_create(1),
        cans:     Vec::new(),
        events:   Vec::new(),
        msg:      Msg::new(),
      }
    }
  }
  /// Adds a can device to the group
  pub fn add(&mut self, can: Can, user_data: T) -> io::Result<()> {
    unsafe {
      for i in 0..self.events.len() {
        if libc::epoll_ctl(self.fd_epoll, libc::EPOLL_CTL_DEL, self.cans[i].can.fd, 0 as *mut libc::epoll_event) != 0 {
          return Err(io::Error::last_os_error());
        }
      }
      self.cans.push(CanData { can: can, user_data: user_data });
      self.events.push(mem::transmute(mem::MaybeUninit::<libc::epoll_event>::uninit()));
      for i in 0..self.events.len() {
        self.events[i].events = libc::EPOLLIN as u32;
        self.events[i].u64 = self.cans.last().unwrap() as *const _ as u64;
        let eptr = self.events.as_mut_ptr();
        if libc::epoll_ctl(self.fd_epoll, libc::EPOLL_CTL_ADD, self.cans[i].can.fd, eptr.add(i)) != 0 {
          return Err(io::Error::last_os_error());
        }
      }
    }
    Ok(())
  }
  /// Receives the the next CAN frame which is avaiable and calls the provided callback. The callback
  /// may be called n times, in which n is the number of added CAN devices. The function blocks either until
  /// at least one CAN devices has new data available, until timeout is reached or until a error happend.
  /// The timeout uses ms granularity. Duration::from_milliseconds(-1) can be passed to this function
  /// to disable the timeout functionallity. The function either returns true, if no timeout happened,
  /// false if a timeout happend or io::Error if an error happend.
  pub fn next(&mut self, timeout: Duration, on_recv: fn(&Box<Msg>, &T)) -> Result<bool, io::Error> {
    unsafe {
      let mut no_timeout = false;
      let num_events = libc::epoll_wait(self.fd_epoll, self.events.as_mut_ptr(), self.events.len() as i32, timeout.num_milliseconds() as i32);
      if num_events == -1 {
        return Err(io::Error::last_os_error());
      }
      if num_events > 0 {
        no_timeout = true;
        for i in 0..num_events {
          let can_data = self.events[i as usize].u64 as *mut CanData<T>;
          (*can_data).can.recv(&mut self.msg)?;
          on_recv(&self.msg, &(*can_data).user_data);
        }
      }
      Ok(no_timeout)
    }
  }
}
impl<T> Drop for CanGroup<T> {
  fn drop(&mut self) {
    unsafe {
      if self.fd_epoll != 0 {
        libc::close(self.fd_epoll);
      }
      self.fd_epoll = 0;
    }
  }
}

#[cfg(test)]
fn on_recv(msg: &Box<Msg>, _user_data: &u64) {
  println!("timestamp: {:?}", msg.timestamp());
  print!("received CAN frame (id: {}): ", msg.can_id());
  for i in 0..msg.len() {
    print!("{} ", msg[i as usize]);
  }
  println!("");
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn it_works() {
    let can = Can::open("vcan0").unwrap();
    let mut cg = CanGroup::<u64>::new();
    cg.add(can, 0).unwrap();
    match cg.next(Duration::milliseconds(-1), on_recv) {
      Ok(no_timeout) => if !no_timeout { panic!("timeout"); },
      Err(_) => panic!("error"),
    }
  }
}

