use std::{
    fmt,
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    thread,
    time::{Duration, Instant},
};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    InvalidArgument(&'static str),
    Protocol(&'static str),
    /// Exception code returned by the device; e.g. 2 means unsupported address.
    Exception(u8),
    /// Reconnect after an I/O or malformed-response error; the stream is unusable.
    Disconnected,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "Modbus TCP I/O: {e}"),
            Self::InvalidArgument(s) => write!(f, "invalid argument: {s}"),
            Self::Protocol(s) => write!(f, "invalid Modbus response: {s}"),
            Self::Exception(code) => write!(f, "Modbus exception 0x{code:02x}"),
            Self::Disconnected => {
                f.write_str("connection unusable; reconnect before sending requests")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// Address 247 selects the whole plant; individual inverters use IDs 1–246.
/// Broadcast (0) is intentionally excluded because it has no response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnitId(u8);

impl UnitId {
    pub const PLANT: Self = Self(247);

    pub fn inverter(id: u8) -> Result<Self> {
        if (1..=246).contains(&id) {
            Ok(Self(id))
        } else {
            Err(Error::InvalidArgument("inverter ID must be 1..=246"))
        }
    }

    pub const fn get(self) -> u8 {
        self.0
    }
}

/// V2.7 §4.1 specifies 0x04 for RO, but §6.1 examples specify 0x03.
/// Default to 0x04; select Holding explicitly for firmware using §6.1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum ReadFunction {
    Holding = 0x03,
    #[default]
    Input = 0x04,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub connect_timeout: Duration,
    /// Deadline for a complete response, not per TCP fragment.
    pub response_timeout: Duration,
    /// At least one second, as specified in §4.2.
    pub request_interval: Duration,
    pub telemetry_function: ReadFunction,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(1),
            response_timeout: Duration::from_secs(1),
            request_interval: Duration::from_secs(1),
            telemetry_function: ReadFunction::Input,
        }
    }
}

/// A serialized, paced TCP connection. Methods block until their response or timeout.
#[derive(Debug)]
pub struct Client {
    stream: TcpStream,
    config: Config,
    transaction: u16,
    last_request: Option<Instant>,
    usable: bool,
    write_source: String,
    auditor: Option<std::sync::Arc<dyn WriteAuditor>>,
}

/// One write transmission event. `attempt` precedes I/O; `sent` means all bytes
/// reached the local TCP stack, not that the device applied them.
#[derive(Clone, Debug)]
pub struct WriteRecord {
    pub phase: &'static str,
    pub target: SocketAddr,
    pub unit: u8,
    pub transaction: u16,
    pub function: u8,
    pub register: u16,
    pub values: Vec<u16>,
    pub frame_hex: String,
    pub source: String,
}

pub trait WriteAuditor: fmt::Debug + Send + Sync {
    fn record(&self, record: &WriteRecord) -> io::Result<()>;
}

impl Client {
    pub fn set_write_auditor(&mut self, auditor: std::sync::Arc<dyn WriteAuditor>) {
        self.auditor = Some(auditor);
    }

    pub fn set_write_source(&mut self, source: impl Into<String>) {
        self.write_source = source.into();
    }

    fn audit_write(
        &self,
        phase: &'static str,
        unit: UnitId,
        pdu: &[u8],
        frame: &[u8],
    ) -> io::Result<()> {
        let values = if pdu[0] == 6 { &pdu[3..5] } else { &pdu[6..] };
        let record = WriteRecord {
            phase,
            target: self.stream.peer_addr()?,
            unit: unit.get(),
            transaction: self.transaction,
            function: pdu[0],
            register: u16::from_be_bytes([pdu[1], pdu[2]]),
            values: values
                .as_chunks::<2>()
                .0
                .iter()
                .map(|v| u16::from_be_bytes([v[0], v[1]]))
                .collect(),
            frame_hex: frame.iter().map(|b| format!("{b:02x}")).collect(),
            source: self.write_source.clone(),
        };
        tracing::debug!(target: "sigen_modbus::write_audit", phase, target_address = %record.target,
            unit_id = record.unit, transaction_id = record.transaction, function = record.function,
            register = record.register, values = ?record.values, frame_hex = %record.frame_hex,
            source = %record.source, "Modbus write");
        if let Some(auditor) = &self.auditor {
            auditor.record(&record)?;
        }
        Ok(())
    }
    pub fn connect(address: SocketAddr) -> Result<Self> {
        Self::connect_with_config(address, Config::default())
    }

    #[tracing::instrument(level = "debug", skip(config), err(level = "debug"))]
    pub fn connect_with_config(address: SocketAddr, config: Config) -> Result<Self> {
        if config.connect_timeout.is_zero() || config.response_timeout.is_zero() {
            return Err(Error::InvalidArgument("timeouts must be positive"));
        }
        if config.request_interval < Duration::from_secs(1) {
            return Err(Error::InvalidArgument(
                "request interval must be at least one second",
            ));
        }
        // Reject durations that cannot be represented as an Instant before any I/O.
        if Instant::now().checked_add(config.connect_timeout).is_none()
            || Instant::now()
                .checked_add(config.response_timeout)
                .is_none()
            || Instant::now()
                .checked_add(config.request_interval)
                .is_none()
        {
            return Err(Error::InvalidArgument("duration is too large"));
        }
        let stream = TcpStream::connect_timeout(&address, config.connect_timeout)?;
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(config.response_timeout))?;
        tracing::debug!(%address, "Modbus TCP connected");
        Ok(Self {
            stream,
            config,
            transaction: 0,
            last_request: None,
            usable: true,
            write_source: "library".into(),
            auditor: None,
        })
    }

    /// Read raw input/holding registers. Addresses are sent literally (no 30001/40001 offset).
    /// Counts follow V2.7's 1–124 limit. Unsupported ranges return a device exception.
    #[tracing::instrument(level = "trace", skip(self), fields(unit_id = unit.get()), err(level = "debug"))]
    pub fn read_registers(
        &mut self,
        unit: UnitId,
        function: ReadFunction,
        address: u16,
        count: u16,
    ) -> Result<Vec<u16>> {
        validate_range(address, count, 124)?;
        let mut request = vec![function as u8];
        request.extend_from_slice(&address.to_be_bytes());
        request.extend_from_slice(&count.to_be_bytes());
        let response = self.exchange(unit, &request)?;
        if response.len() != 2 + count as usize * 2 || response[1] as u16 != count * 2 {
            return self.bad_response("register byte count does not match request");
        }
        Ok(response[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|r| u16::from_be_bytes([r[0], r[1]]))
            .collect())
    }

    pub(crate) fn telemetry(&mut self, unit: UnitId, address: u16, count: u16) -> Result<Vec<u16>> {
        self.read_registers(unit, self.config.telemetry_function, address, count)
    }

    /// FC 0x06. Caller must select a writable register and obey its model/mode constraints.
    /// A timeout may occur after a write was applied; read back before deciding to retry.
    #[tracing::instrument(level = "debug", skip(self), fields(unit_id = unit.get()), err(level = "debug"))]
    pub fn write_register(&mut self, unit: UnitId, address: u16, value: u16) -> Result<()> {
        self.write_register_checked(unit, address, value, || Ok(()))
    }

    /// Like `write_register`, but checks authorization/cancellation after pacing,
    /// immediately before sending. An error from `check` sends no bytes.
    /// Once transmission begins, cancellation cannot undo a hardware write.
    pub fn write_register_checked(
        &mut self,
        unit: UnitId,
        address: u16,
        value: u16,
        check: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let mut request = vec![0x06];
        request.extend_from_slice(&address.to_be_bytes());
        request.extend_from_slice(&value.to_be_bytes());
        let response = self.exchange_checked(unit, &request, check)?;
        if response != request {
            return self.bad_response("single-write echo mismatch");
        }
        Ok(())
    }

    /// FC 0x10, for atomic multiword values. Caller is responsible for register permissions.
    /// Never automatically retried; see `write_register` for uncertain write outcomes.
    #[tracing::instrument(level = "debug", skip(self, values), fields(unit_id = unit.get(), count = values.len()), err(level = "debug"))]
    pub fn write_registers(&mut self, unit: UnitId, address: u16, values: &[u16]) -> Result<()> {
        let count = u16::try_from(values.len())
            .map_err(|_| Error::InvalidArgument("too many registers"))?;
        validate_range(address, count, 123)?;
        let mut request = vec![0x10];
        request.extend_from_slice(&address.to_be_bytes());
        request.extend_from_slice(&count.to_be_bytes());
        request.push((count * 2) as u8);
        for value in values {
            request.extend_from_slice(&value.to_be_bytes());
        }
        let response = self.exchange(unit, &request)?;
        if response != request[..5] {
            return self.bad_response("multiple-write echo mismatch");
        }
        Ok(())
    }

    fn bad_response<T>(&mut self, message: &'static str) -> Result<T> {
        tracing::debug!(
            reason = message,
            "Closing Modbus TCP connection after invalid response"
        );
        self.usable = false;
        let _ = self.stream.shutdown(Shutdown::Both);
        Err(Error::Protocol(message))
    }

    fn exchange(&mut self, unit: UnitId, pdu: &[u8]) -> Result<Vec<u8>> {
        self.exchange_checked(unit, pdu, || Ok(()))
    }

    fn exchange_checked(
        &mut self,
        unit: UnitId,
        pdu: &[u8],
        check: impl FnOnce() -> Result<()>,
    ) -> Result<Vec<u8>> {
        if !self.usable {
            return Err(Error::Disconnected);
        }
        if let Some(last) = self.last_request {
            thread::sleep(self.config.request_interval.saturating_sub(last.elapsed()));
        }
        self.transaction = self.transaction.wrapping_add(1);
        let _span = tracing::trace_span!(
            "modbus_request",
            transaction_id = self.transaction,
            unit_id = unit.get(),
            function = pdu[0]
        )
        .entered();
        let started = Instant::now();
        let mut frame = Vec::with_capacity(7 + pdu.len());
        frame.extend_from_slice(&self.transaction.to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(&(pdu.len() as u16 + 1).to_be_bytes());
        frame.push(unit.get());
        frame.extend_from_slice(pdu);
        tracing::trace!(request_bytes = pdu.len(), "Sending Modbus request");
        let result = (|| {
            let writing = matches!(pdu[0], 6 | 0x10);
            if writing {
                self.audit_write("attempt", unit, pdu, &frame)?;
            }
            check()?;
            self.last_request = Some(Instant::now());
            if let Err(error) = self.stream.write_all(&frame) {
                if writing {
                    let _ = self.audit_write("send_failed", unit, pdu, &frame);
                }
                return Err(Error::Io(error));
            }
            if writing {
                self.audit_write("sent", unit, pdu, &frame)?;
            }
            let deadline = Instant::now() + self.config.response_timeout;
            let mut header = [0; 7];
            read_until(&mut self.stream, &mut header, deadline)?;
            if header[..2] != self.transaction.to_be_bytes() {
                return Err(Error::Protocol("transaction ID mismatch"));
            }
            if header[2..4] != [0, 0] {
                return Err(Error::Protocol("nonzero protocol ID"));
            }
            if header[6] != unit.get() {
                return Err(Error::Protocol("unit ID mismatch"));
            }
            let length = u16::from_be_bytes([header[4], header[5]]) as usize;
            if !(3..=254).contains(&length) {
                return Err(Error::Protocol("invalid MBAP length"));
            }
            let mut response = vec![0; length - 1];
            read_until(&mut self.stream, &mut response, deadline)?;
            if response[0] == (pdu[0] | 0x80) {
                if response.len() != 2 {
                    return Err(Error::Protocol("invalid exception length"));
                }
                return Err(Error::Exception(response[1]));
            }
            if response[0] != pdu[0] {
                return Err(Error::Protocol("function code mismatch"));
            }
            Ok(response)
        })();
        match &result {
            Ok(response) => tracing::trace!(
                response_bytes = response.len(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "Modbus response received"
            ),
            Err(error) => {
                tracing::debug!(%error, elapsed_ms = started.elapsed().as_millis() as u64, "Modbus request failed")
            }
        }
        if matches!(result, Err(Error::Io(_) | Error::Protocol(_))) {
            self.usable = false;
            let _ = self.stream.shutdown(Shutdown::Both);
        }
        result
    }
}

fn validate_range(address: u16, count: u16, max: u16) -> Result<()> {
    if count == 0 || count > max || address as u32 + count as u32 > 65536 {
        return Err(Error::InvalidArgument("invalid register range or count"));
    }
    Ok(())
}

fn read_until(stream: &mut TcpStream, mut buffer: &mut [u8], deadline: Instant) -> io::Result<()> {
    while !buffer.is_empty() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        stream.set_read_timeout(Some(remaining))?;
        match stream.read(buffer) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => buffer = &mut buffer[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
