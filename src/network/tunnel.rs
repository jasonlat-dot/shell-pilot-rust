use super::*;
use base64::Engine;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
fn failed() -> Error {
    invalid("代理连接或认证失败，请检查代理地址、端口、账号及代理是否运行")
}
pub(super) async fn connect(host: &str, port: u16, route: &Route) -> Result<TcpStream, Error> {
    let Some(proxy) = route else {
        return TcpStream::connect((host.trim_matches(['[', ']']), port))
            .await
            .map_err(|_| Error::Ssh);
    };
    let mut socket = TcpStream::connect((proxy.host.as_str(), proxy.port))
        .await
        .map_err(|_| failed())?;
    if proxy.protocol == Protocol::Http {
        http(&mut socket, host, port, proxy).await?;
    } else {
        socks(&mut socket, host, port, proxy).await?;
    }
    Ok(socket)
}
async fn http(
    socket: &mut TcpStream,
    host: &str,
    port: u16,
    proxy: &Endpoint,
) -> Result<(), Error> {
    if host.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(failed());
    }
    let authority = if host.contains(':') {
        format!("[{}]:{port}", host.trim_matches(['[', ']']))
    } else {
        format!("{host}:{port}")
    };
    let auth = if proxy.username.is_empty() {
        String::new()
    } else {
        format!(
            "Proxy-Authorization: Basic {}\r\n",
            base64::engine::general_purpose::STANDARD
                .encode(format!("{}:{}", proxy.username, proxy.password))
        )
    };
    socket
        .write_all(
            format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n{auth}\r\n").as_bytes(),
        )
        .await
        .map_err(|_| failed())?;
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= 8192 {
            return Err(failed());
        }
        header.push(socket.read_u8().await.map_err(|_| failed())?);
    }
    let first = std::str::from_utf8(&header)
        .map_err(|_| failed())?
        .lines()
        .next()
        .ok_or_else(failed)?;
    let mut parts = first.split_whitespace();
    if !matches!(parts.next(), Some("HTTP/1.0" | "HTTP/1.1"))
        || parts
            .next()
            .and_then(|v| v.parse::<u16>().ok())
            .is_none_or(|status| !(200..300).contains(&status))
    {
        return Err(failed());
    }
    Ok(())
}
async fn socks(
    socket: &mut TcpStream,
    host: &str,
    port: u16,
    proxy: &Endpoint,
) -> Result<(), Error> {
    let authenticated = !proxy.username.is_empty();
    socket
        .write_all(&[5, 1, if authenticated { 2 } else { 0 }])
        .await
        .map_err(|_| failed())?;
    let mut reply = [0u8; 2];
    socket.read_exact(&mut reply).await.map_err(|_| failed())?;
    if reply != [5, if authenticated { 2 } else { 0 }] {
        return Err(failed());
    }
    if authenticated {
        let mut auth = vec![1, proxy.username.len() as u8];
        auth.extend_from_slice(proxy.username.as_bytes());
        auth.push(proxy.password.len() as u8);
        auth.extend_from_slice(proxy.password.as_bytes());
        socket.write_all(&auth).await.map_err(|_| failed())?;
        socket.read_exact(&mut reply).await.map_err(|_| failed())?;
        if reply != [1, 0] {
            return Err(failed());
        }
    }
    let host = host.trim_matches(['[', ']']);
    let mut request = vec![5, 1, 0];
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            request.push(1);
            request.extend_from_slice(&ip.octets());
        }
        Ok(IpAddr::V6(ip)) => {
            request.push(4);
            request.extend_from_slice(&ip.octets());
        }
        Err(_) => {
            if host.is_empty() || host.len() > 255 {
                return Err(failed());
            }
            request.extend_from_slice(&[3, host.len() as u8]);
            request.extend_from_slice(host.as_bytes());
        }
    }
    request.extend_from_slice(&port.to_be_bytes());
    socket.write_all(&request).await.map_err(|_| failed())?;
    let mut response = [0u8; 4];
    socket
        .read_exact(&mut response)
        .await
        .map_err(|_| failed())?;
    if response[..3] != [5, 0, 0] {
        return Err(failed());
    }
    let bytes = match response[3] {
        1 => 4,
        4 => 16,
        3 => socket.read_u8().await.map_err(|_| failed())? as usize,
        _ => return Err(failed()),
    };
    let mut address = vec![0u8; bytes + 2];
    socket
        .read_exact(&mut address)
        .await
        .map_err(|_| failed())?;
    Ok(())
}
