//! Modbus client — TCP transport with function codes 0x03, 0x04, 0x06, 0x10.

pub mod tcp;

pub use tcp::TcpTransport;

const MAX_REGISTERS: u16 = 125;

#[derive(Debug)]
pub enum ModbusError {
    ConnectionFailed(String),
    Timeout,
    InvalidResponse(String),
    Exception(u8),
    Io(std::io::Error),
}

impl std::fmt::Display for ModbusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModbusError::ConnectionFailed(e) => write!(f, "connection failed: {}", e),
            ModbusError::Timeout => write!(f, "timeout"),
            ModbusError::InvalidResponse(e) => write!(f, "invalid response: {}", e),
            ModbusError::Exception(code) => write!(f, "modbus exception: 0x{:02x}", code),
            ModbusError::Io(e) => write!(f, "io: {}", e),
        }
    }
}

impl From<std::io::Error> for ModbusError {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::TimedOut || e.kind() == std::io::ErrorKind::WouldBlock {
            ModbusError::Timeout
        } else {
            ModbusError::Io(e)
        }
    }
}

/// Modbus TCP client.
pub struct ModbusClient {
    transport: TcpTransport,
}

impl ModbusClient {
    /// Connect via Modbus TCP.
    pub fn connect(addr: &str, port: u16, unit_id: u8) -> Result<Self, ModbusError> {
        let transport = TcpTransport::connect(addr, port, unit_id)?;
        Ok(Self { transport })
    }

    /// Read holding registers (function code 0x03).
    pub fn read_holding_registers(
        &mut self,
        start: u16,
        count: u16,
    ) -> Result<Vec<u16>, ModbusError> {
        self.read_registers(0x03, start, count)
    }

    /// Read input registers (function code 0x04).
    pub fn read_input_registers(
        &mut self,
        start: u16,
        count: u16,
    ) -> Result<Vec<u16>, ModbusError> {
        self.read_registers(0x04, start, count)
    }

    /// Write a single register (function code 0x06).
    pub fn write_register(&mut self, addr: u16, value: u16) -> Result<(), ModbusError> {
        let mut pdu = [0u8; 5];
        pdu[0] = 0x06;
        pdu[1..3].copy_from_slice(&addr.to_be_bytes());
        pdu[3..5].copy_from_slice(&value.to_be_bytes());

        let response = self.transport.send_request(&pdu)?;
        if response.len() < 5 || response[0] != 0x06 {
            return Err(ModbusError::InvalidResponse("bad write response".into()));
        }
        Ok(())
    }

    /// Write multiple consecutive registers (function code 0x10).
    pub fn write_multiple_registers(
        &mut self,
        start: u16,
        values: &[u16],
    ) -> Result<(), ModbusError> {
        if values.is_empty() || values.len() > MAX_REGISTERS as usize {
            return Err(ModbusError::InvalidResponse(format!(
                "count {} out of range 1-{}",
                values.len(),
                MAX_REGISTERS
            )));
        }

        let count = values.len() as u16;
        let byte_count = (values.len() * 2) as u8;
        let mut pdu = Vec::with_capacity(6 + values.len() * 2);
        pdu.push(0x10);
        pdu.extend_from_slice(&start.to_be_bytes());
        pdu.extend_from_slice(&count.to_be_bytes());
        pdu.push(byte_count);
        for v in values {
            pdu.extend_from_slice(&v.to_be_bytes());
        }

        let response = self.transport.send_request(&pdu)?;
        if response.len() < 5 || response[0] != 0x10 {
            // Check for exception
            if !response.is_empty() && response[0] == 0x90 {
                let exc = if response.len() > 1 { response[1] } else { 0 };
                return Err(ModbusError::Exception(exc));
            }
            return Err(ModbusError::InvalidResponse("bad write_multiple response".into()));
        }
        Ok(())
    }

    fn read_registers(
        &mut self,
        function_code: u8,
        start: u16,
        count: u16,
    ) -> Result<Vec<u16>, ModbusError> {
        if count == 0 || count > MAX_REGISTERS {
            return Err(ModbusError::InvalidResponse(format!(
                "count {} out of range 1-{}",
                count, MAX_REGISTERS
            )));
        }

        let mut pdu = [0u8; 5];
        pdu[0] = function_code;
        pdu[1..3].copy_from_slice(&start.to_be_bytes());
        pdu[3..5].copy_from_slice(&count.to_be_bytes());

        let response = self.transport.send_request(&pdu)?;

        if response.is_empty() {
            return Err(ModbusError::InvalidResponse("empty response".into()));
        }

        // Check for exception response (function code + 0x80)
        if response[0] == function_code | 0x80 {
            let exception_code = if response.len() > 1 { response[1] } else { 0 };
            return Err(ModbusError::Exception(exception_code));
        }

        if response[0] != function_code {
            return Err(ModbusError::InvalidResponse(format!(
                "expected function 0x{:02x}, got 0x{:02x}",
                function_code, response[0]
            )));
        }

        if response.len() < 2 {
            return Err(ModbusError::InvalidResponse("missing byte count".into()));
        }

        let byte_count = response[1] as usize;
        let expected = count as usize * 2;
        if byte_count != expected || response.len() < 2 + byte_count {
            return Err(ModbusError::InvalidResponse(format!(
                "expected {} bytes, got {}",
                expected, byte_count
            )));
        }

        let mut values = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            let hi = response[2 + i * 2] as u16;
            let lo = response[3 + i * 2] as u16;
            values.push((hi << 8) | lo);
        }

        Ok(values)
    }
}
