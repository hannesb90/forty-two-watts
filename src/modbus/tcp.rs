//! Modbus TCP transport -- MBAP header framing over TCP.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use super::ModbusError;

const TIMEOUT: Duration = Duration::from_secs(5);
const MBAP_HEADER_LEN: usize = 7;

/// Modbus TCP transport.
pub struct TcpTransport {
    stream: TcpStream,
    transaction_id: u16,
    unit_id: u8,
}

impl TcpTransport {
    /// Connect to a Modbus TCP server.
    pub fn connect(addr: &str, port: u16, unit_id: u8) -> Result<Self, ModbusError> {
        let target = format!("{}:{}", addr, port);
        let stream = TcpStream::connect_timeout(
            &target
                .parse()
                .map_err(|e| ModbusError::ConnectionFailed(format!("{}", e)))?,
            TIMEOUT,
        )
        .map_err(|e| ModbusError::ConnectionFailed(e.to_string()))?;

        stream.set_read_timeout(Some(TIMEOUT)).ok();
        stream.set_write_timeout(Some(TIMEOUT)).ok();
        stream.set_nodelay(true).ok();

        Ok(Self {
            stream,
            transaction_id: 0,
            unit_id,
        })
    }

    /// Send a PDU and receive the response PDU (MBAP framed).
    pub fn send_request(&mut self, pdu: &[u8]) -> Result<Vec<u8>, ModbusError> {
        self.transaction_id = self.transaction_id.wrapping_add(1);

        // MBAP header: transaction_id(2) + protocol_id(2) + length(2) + unit_id(1)
        let length = (pdu.len() + 1) as u16;
        let mut frame = Vec::with_capacity(MBAP_HEADER_LEN + pdu.len());
        frame.extend_from_slice(&self.transaction_id.to_be_bytes());
        frame.extend_from_slice(&0u16.to_be_bytes()); // Protocol ID = 0
        frame.extend_from_slice(&length.to_be_bytes());
        frame.push(self.unit_id);
        frame.extend_from_slice(pdu);

        self.stream.write_all(&frame)?;

        // Read response MBAP header
        let mut header = [0u8; MBAP_HEADER_LEN];
        self.stream.read_exact(&mut header)?;

        let resp_length = u16::from_be_bytes([header[4], header[5]]) as usize;
        if resp_length < 1 || resp_length > 260 {
            return Err(ModbusError::InvalidResponse(format!(
                "invalid response length: {}",
                resp_length
            )));
        }

        // Read PDU (response_length - 1 for unit_id already counted)
        let pdu_len = resp_length - 1;
        let mut response_pdu = vec![0u8; pdu_len];
        self.stream.read_exact(&mut response_pdu)?;

        Ok(response_pdu)
    }

    /// Get the unit ID.
    pub fn unit_id(&self) -> u8 {
        self.unit_id
    }
}
