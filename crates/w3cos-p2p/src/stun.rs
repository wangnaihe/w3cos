//! STUN Binding (RFC 5389) against a list of public servers.
//! The reflexive address is only a candidate. AutoNAT and the peer's
//! observed address are the other inputs.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::time::Duration;

const MAGIC: u32 = 0x2112_A442;
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_RESPONSE: u16 = 0x0101;
const XOR_MAPPED: u16 = 0x0020;

pub fn binding_request(transaction_id: &[u8; 12]) -> [u8; 20] {
    let mut out = [0u8; 20];
    out[0..2].copy_from_slice(&BINDING_REQUEST.to_be_bytes());
    out[4..8].copy_from_slice(&MAGIC.to_be_bytes());
    out[8..20].copy_from_slice(transaction_id);
    out
}

pub fn parse_binding_response(packet: &[u8], transaction_id: &[u8; 12]) -> Option<SocketAddr> {
    if packet.len() < 20 {
        return None;
    }
    let kind = u16::from_be_bytes([packet[0], packet[1]]);
    if kind != BINDING_RESPONSE {
        return None;
    }
    let magic = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
    if magic != MAGIC || &packet[8..20] != transaction_id {
        return None;
    }
    let mut offset = 20usize;
    while offset + 4 <= packet.len() {
        let attr = u16::from_be_bytes([packet[offset], packet[offset + 1]]);
        let len = u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]) as usize;
        offset += 4;
        if offset + len > packet.len() {
            return None;
        }
        let value = &packet[offset..offset + len];
        offset += (len + 3) & !3;
        if attr == XOR_MAPPED {
            return decode_xor(value, transaction_id);
        }
    }
    None
}

fn decode_xor(value: &[u8], transaction_id: &[u8; 12]) -> Option<SocketAddr> {
    if value.len() < 8 {
        return None;
    }
    let family = value[1];
    let xport = u16::from_be_bytes([value[2], value[3]]);
    let port = xport ^ (MAGIC >> 16) as u16;
    let ip = match family {
        0x01 => {
            if value.len() < 8 {
                return None;
            }
            let raw = u32::from_be_bytes([value[4], value[5], value[6], value[7]]);
            IpAddr::V4(Ipv4Addr::from(raw ^ MAGIC))
        }
        0x02 => {
            if value.len() < 20 {
                return None;
            }
            let mut addr = [0u8; 16];
            addr.copy_from_slice(&value[4..20]);
            let magic = MAGIC.to_be_bytes();
            for (index, byte) in addr.iter_mut().enumerate() {
                let mask = if index < 4 {
                    magic[index]
                } else {
                    transaction_id[index - 4]
                };
                *byte ^= mask;
            }
            IpAddr::V6(Ipv6Addr::from(addr))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

/// Asks each server in order. The first reflexive address wins.
pub fn probe(servers: &[String], timeout: Duration) -> Option<SocketAddr> {
    if servers.is_empty() {
        return None;
    }
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.set_read_timeout(Some(timeout)).ok()?;
    let mut transaction_id = [0u8; 12];
    let _ = getrandom::getrandom(&mut transaction_id);
    let request = binding_request(&transaction_id);
    for server in servers {
        let target = if let Ok(addr) = server.parse::<SocketAddr>() {
            addr
        } else if let Ok(addr) = resolve_host(server) {
            addr
        } else {
            continue;
        };
        if socket.send_to(&request, target).is_err() {
            continue;
        }
        let mut buf = [0u8; 512];
        if let Ok((size, _)) = socket.recv_from(&mut buf)
            && let Some(addr) = parse_binding_response(&buf[..size], &transaction_id)
        {
            return Some(addr);
        }
    }
    None
}

fn resolve_host(server: &str) -> Result<SocketAddr, ()> {
    use std::net::ToSocketAddrs;
    server
        .to_socket_addrs()
        .map_err(|_| ())?
        .find(|addr| addr.is_ipv4())
        .ok_or(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xor_mapped_address_roundtrips_a_fixture() {
        let id = [9u8; 12];
        let request = binding_request(&id);
        assert_eq!(&request[0..2], &BINDING_REQUEST.to_be_bytes());
        let ip = Ipv4Addr::new(203, 0, 113, 9);
        let port = 40000u16;
        let xport = port ^ (MAGIC >> 16) as u16;
        let xip = u32::from(ip) ^ MAGIC;
        let mut packet = Vec::from(request);
        packet[0..2].copy_from_slice(&BINDING_RESPONSE.to_be_bytes());
        packet.extend_from_slice(&XOR_MAPPED.to_be_bytes());
        packet.extend_from_slice(&8u16.to_be_bytes());
        packet.push(0);
        packet.push(0x01);
        packet.extend_from_slice(&xport.to_be_bytes());
        packet.extend_from_slice(&xip.to_be_bytes());
        let parsed = parse_binding_response(&packet, &id).expect("mapped");
        assert_eq!(parsed, SocketAddr::from((ip, port)));
        assert!(parse_binding_response(&packet, &[1u8; 12]).is_none());
    }
}
