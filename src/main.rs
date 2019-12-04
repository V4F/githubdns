//! A "tiny" example of HTTP request/response handling using transports.
//!
//! This example is intended for *learning purposes* to see how various pieces
//! hook up together and how HTTP can get up and running. Note that this example
//! is written with the restriction that it *can't* use any "big" library other
//! than Tokio, if you'd like a "real world" HTTP library you likely want a
//! crate like Hyper.
//!
//! Code here is based on the `echo-threads` example and implements two paths,
//! the `/plaintext` and `/json` routes to respond with some text and json,
//! respectively. By default this will run I/O on all the cores your system has
//! available, and it doesn't support HTTP request bodies.

#![warn(rust_2018_idioms)]

use futures::future::*;
use native_tls;
use scraper::{Html, Selector};
use std::collections::HashMap;
use std::error::Error;

use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};
use std::sync::{Arc, RwLock};

use clap::{App, Arg};
use std::process::Command;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    //    let domains = [
    //        "github.com",
    //        "github.global.ssl.fastly.net",
    //        "codeload.github.com",
    //    ];
    let m = App::new("githubdns")
        .arg(
            Arg::with_name("domains")
                .long("domains")
                .help("set the dns needs to write to hosts")
                .default_value("github.com,github.global.ssl.fastly.net,codeload.github.com,assets-cdn.github.com")
                .required(false),
        )
        .get_matches();
    let domains_str = m.value_of("domains").unwrap().to_string();
    let domains: Vec<&str> = domains_str.split(",").collect();
    let domain_ips = Arc::new(RwLock::new(HashMap::new()));
    if true {
        let mut v = Vec::new();
        for domain in domains.iter() {
            let domain_ips = domain_ips.clone();
            let domain = String::from(*domain);
            v.push(async move {
                let res = get(domain.clone()).await;
                if res.is_err() {
                    println!(
                        "get domain {},err {}",
                        domain,
                        res.unwrap_err().description()
                    );
                    return;
                }
                let res = res.unwrap();
                let ip = get_address(&res, domain.clone());
                domain_ips.write().unwrap().insert(domain.clone(), ip);
            });
        }
        join_all(v).await;
    } else {
        let mut dm = domain_ips.write().unwrap();
        for domain in domains {
            dm.insert(String::from(domain), "127.0.0.1".into());
        }
    }
    //    println!("domain_ips={:?}", domain_ips.read().unwrap());
    let (hosts_file, enter) = get_hosts_file();
    read_and_modify_hosts(domain_ips.clone(), &hosts_file, &enter).await;
    Ok(())
}
//hosts文件路径,以及回车换行对应的是\r\n还是\n,\r
//这里有一个副作用,会讲hosts文件的只读属性移除,可以考虑写入后再增加上,
fn get_hosts_file() -> (String, String) {
    let info = sys_info::os_type();
    if info.is_err() {
        panic!("unsupported os");
    }
    let info = info.unwrap();
    //    Such as "Linux", "Darwin", "Windows".
    match info.as_str() {
        "Linux" => ("/etc/hosts".into(), "\n".into()),
        "Darwin" => ("/etc/hosts".into(), "\r".into()),
        "Windows" => {
            let path = r"C:\Windows\System32\drivers\etc\hosts";
            //windows下hosts文件默认是只读的,如果不修改,后续会写不进去
            Command::new("attrib")
                .args(&["-R", path])
                .output()
                .expect("remove readonly failed");

            (path.into(), "\r\n".into())
        }
        _ => panic!("not supported os {}", info),
    }
}
//读取hosts文件,如果其中已经有相关域名的设置,先删除,再添加
//原有注释保持不动
async fn read_and_modify_hosts(
    m: Arc<RwLock<HashMap<String, String>>>,
    hosts_file: &str,
    enter: &str,
) {
    use tokio::fs;
    let flags = "# ----Generated By githubdns ---";
    let hosts_file_name = hosts_file;
    let mut m = m.write().unwrap();
    let contents = fs::read(hosts_file_name).await;
    if contents.is_err() {
        println!(
            "read {} err {}",
            hosts_file_name,
            contents.err().unwrap().description()
        );
        return;
    }
    let contents = contents.unwrap();
    let s = String::from_utf8(contents).unwrap();
    let mut lines: Vec<&str> = s.split(enter).collect();
    let mut i = 0;
    while i < lines.len() {
        let l = lines.get(i).unwrap().clone();
        if l == flags {
            lines.remove(i);
            continue;
        }
        if l.trim_start().starts_with("#") {
            i += 1;
            continue; //注释行
        }
        let _ = m.iter().any(|(domain, ip)| {
            let pos = l.find(domain.as_str());
            if pos.is_some() {
                //如果是这个domain的子域名,也不关心
                let pos = pos.unwrap();
                if ip.len() > 0
                    && pos > 0
                    && (l.as_bytes()[pos - 1] == ' ' as u8 || l.as_bytes()[pos - 1] == '\t' as u8)
                {
                    //是我们要找的完整的域名
                    lines.remove(i);
                    i -= 1;
                    return true;
                }
            }
            return false;
        });
        i += 1;
    }

    let mut lines: Vec<_> = lines.iter().map(|n| String::from(*n)).collect();

    lines.push(flags.into());
    for (domain, ip) in m.iter_mut() {
        if ip.len() > 0 {
            lines.push(format!("{}\t {}", ip, domain));
        }
    }
    lines.push(flags.into());
    let r = fs::write(hosts_file, lines.join(enter).as_bytes()).await;
    if r.is_err() {
        panic!("write to {} ,err={}", hosts_file, r.unwrap_err());
    }
}
//解析url,返回对应的domain和path
fn parse_url(domain: &str) -> (String, String) {
    let ss: Vec<_> = domain.split(".").collect();
    let mut path = "/".into();
    let mut domain: String = domain.into();
    if ss.len() > 2 {
        path = format!("/{}", domain.clone());
        domain = ss[ss.len() - 2..].join(".");
    }
    domain = format!("{}.ipaddress.com", domain);
    return (domain, path);
}

//根据url,获取其地址对应的html内容
async fn get(domain: String) -> Result<String, Box<dyn Error>> {
    let (domain, path) = parse_url(domain.as_str());
    println!("get {},{}", domain, path);
    // First up, resolve google.com
    let ip_port = format!("{}:443", domain.clone());
    //    println!("ip_port={}", ip_port);
    let addr = ip_port.to_socket_addrs().unwrap().next().unwrap();
    //    println!("addr={}", addr);
    let socket = TcpStream::connect(&addr).await.unwrap();
    // Send off the request by first negotiating an SSL handshake, then writing
    // of our request, then flushing, then finally read off the response.
    let builder = native_tls::TlsConnector::builder();
    let connector = builder.build().unwrap();
    let connector = tokio_tls::TlsConnector::from(connector);
    let mut socket = connector.connect(domain.as_str(), socket).await?;
    socket
        .write_all(format!("GET {} HTTP/1.0\r\nHost:{}\r\n\r\n", path, domain).as_bytes())
        .await?;

    let mut data = Vec::new();
    socket.read_to_end(&mut data).await?;
    let s = String::from_utf8(data)?;
    let pos = s.find("\r\n\r\n").unwrap_or(0);
    let (_, body) = s.split_at(pos);
    //    println!("body={}", body);
    Ok(String::from(body))
}
//从html中提取domain对应的第一个ipv4地址
fn get_address(data: &str, domain: String) -> String {
    let document = Html::parse_document(data);
    let ul_selector = Selector::parse("ul.comma-separated").unwrap();
    let li_selector = Selector::parse("li").unwrap();
    let ul = document.select(&ul_selector).next();
    if ul.is_none() {
        println!("{} cannot found ul,data={}", domain, data);
        return String::new();
    }
    let ul = ul.unwrap();
    let mut ip_v4 = Ipv4Addr::new(127, 0, 0, 1);
    let found = ul.select(&li_selector).any(|n| {
        //        println!("n={}", n.inner_html().trim());
        let ip: Result<IpAddr, _> = n.inner_html().trim().parse();
        match ip {
            Err(_) => {
                return false;
            }
            Ok(ip) => match ip {
                IpAddr::V4(ipv4) => {
                    ip_v4 = ipv4;
                    return true;
                }
                _ => {
                    return false;
                }
            },
        }
    });
    if found {
        return ip_v4.to_string();
    }
    return String::new(); //没有找到就返回空
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn test_parse_url() {
        let u = parse_url("github.global.ssl.fastly.net");
        assert_eq!(
            u,
            (
                String::from("fastly.net.ipaddress.com"),
                String::from("/github.global.ssl.fastly.net")
            )
        );
        let u = parse_url("github.com");
        assert_eq!(
            u,
            (String::from("github.com.ipaddress.com"), String::from("/"))
        );
    }
    #[tokio::test]
    async fn test_get() {
        assert!(true);
    }
    #[test]
    fn test_get_address() {
        let data = std::fs::read_to_string("github.com.html").unwrap();
        let ip = get_address(data.as_str(), String::from("github.com"));
        assert_eq!(ip, String::from("192.30.253.112"))
    }
}
