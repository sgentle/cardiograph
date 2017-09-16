extern crate bytes;
extern crate futures;
extern crate tokio_core;
extern crate tokio_io;
extern crate tokio_serial;
extern crate tokio_signal;

mod andselect;
mod sample;

use std::{env, io, str};
use tokio_core::reactor::Core;

use tokio_io::codec::{Decoder, Encoder, FramedRead};
use tokio_io::AsyncRead;
use bytes::BytesMut;

use futures::{stream, Future, Stream};

struct PlotterCodec;

impl Decoder for PlotterCodec {
    type Item = String;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let newline = src.as_ref().iter().position(|b| *b == b'\n');
        if let Some(n) = newline {
            let line = src.split_to(n + 1);
            return match str::from_utf8(line.as_ref()) {
                Ok(s) => Ok(Some(s.to_string())),
                Err(_) => Err(io::Error::new(io::ErrorKind::Other, "Invalid String")),
            };
        }
        Ok(None)
    }
}

impl Encoder for PlotterCodec {
    type Item = String;
    type Error = io::Error;

    fn encode(&mut self, msg: Self::Item, buf: &mut BytesMut) -> Result<(), Self::Error> {
        buf.extend(msg.as_bytes());
        buf.extend(b"\n");

        Ok(())
    }
}

struct ECGCodec;

fn makeval(high: u8, low: u8) -> u32 {
    u32::from(high) * 256 + u32::from(low)
}

impl Decoder for ECGCodec {
    type Item = u32;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let sync = src.as_ref().windows(2).position(|v| v == [0xa5, 0x5a]);
        if let Some(n) = sync {
            let packet = src.split_to(n + 2);

            if n == 5 {
                return Ok(Some(makeval(packet[2], packet[3])));
            } else {
                return Ok(None);
            }
        }
        Ok(None)
    }
}

// impl Encoder for ECGCodec {
//     type Item = String;
//     type Error = io::Error;

//     fn encode(&mut self, msg: Self::Item, buf: &mut BytesMut) -> Result<(), Self::Error> {

//         buf.extend(msg.as_bytes());
//         buf.extend(b"\n");

//         Ok(())
//     }
// }

const AVG_WINDOW: usize = 10;

pub fn clamp(val: i32, min: i32, max: i32) -> i32 {
    assert!(min <= max);
    let mut x = val;
    if x < min {
        x = min;
    }
    if x > max {
        x = max;
    }
    x
}

fn main() {
    let mut core = Core::new().unwrap();
    let handle = core.handle();
    let mut args = env::args();

    let ecg_tty_path = args.nth(1).unwrap_or_else(|| "/dev/ttyUSB0".into());
    let plotter_tty_path = args.nth(0).unwrap_or_else(|| "/dev/ttyUSB1".into());

    println!("ecg @ {}, plotter @ {}", ecg_tty_path, plotter_tty_path);

    let plotter_settings = tokio_serial::SerialPortSettings::default();
    let mut plotter_port =
        tokio_serial::Serial::from_path(plotter_tty_path, &plotter_settings, &handle).unwrap();

    let mut ecg_settings = tokio_serial::SerialPortSettings::default();
    ecg_settings.baud_rate = tokio_serial::BaudRate::Baud115200;
    let mut ecg_port =
        tokio_serial::Serial::from_path(ecg_tty_path, &ecg_settings, &handle).unwrap();


    plotter_port
        .set_exclusive(false)
        .expect("Unable to set plotter serial port exlusive");

    let (plotter_writer, plotter_reader) = plotter_port.framed(PlotterCodec).split();

    let plotter_ok = plotter_reader.filter(|x| x == "OK\r\n");

    ecg_port
        .set_exclusive(false)
        .expect("Unable to set ecg serial port exlusive");

    let ecg_reader = FramedRead::new(ecg_port, ECGCodec);


    let mut avgs: [u32; AVG_WINDOW] = [0; AVG_WINDOW];
    let mut avgi = 0;

    let ecg_avg = ecg_reader.map(move |val| {
        avgs[avgi] = val as u32;
        avgi = (avgi + 1) % AVG_WINDOW;

        avgs.iter().sum::<u32>() / (AVG_WINDOW as u32)
    });


    let mut y: i32 = 0;
    let mut yv: i32 = 0;
    let mut ya: i32 = 0;

    let t = 4;
    let maxy: i32 = 1000;
    let maxv: i32 = t * 20;
    let maxa: i32 = 5;
    let jerk: i32 = 5;


    let plotter_targets = ecg_avg.map(|avg| {
        // println!("{}=", " ".repeat((avg / 20) as usize));
        let float = ((avg as i32) - 512) as f32 / 1024.0;
        let scaled = (float * maxy as f32) as i32;
        scaled
    });


    let mut targety: i32 = 0;
    let mut nexty: i32 = 0;
    let plotter_control = sample::new(plotter_targets, plotter_ok).map(|aftery| {
        println!(
            "{}*",
            " ".repeat((targety + maxy) as usize * 80 / maxy as usize)
        );
        let targetv = targety - y;
        let nextv = nexty - targety;
        let afterv = aftery - nexty;

        let targeta = (targetv + nextv * 2) / 3 - yv;
        let nexta = afterv - nextv;

        let targetj = (targeta * 2 + nexta) / 3 - ya;

        let yj = clamp(targetj, -jerk, jerk);
        ya = clamp(ya + yj, -maxa, maxa);
        yv = clamp(yv + ya, -maxv, maxv);
        y = clamp(y + yv, -maxy, maxy);

        targety = nexty;
        nexty = aftery;

        // println!("y:{} ty:{} v:{} tv:{} a:{} ta:{}", y, yv, ya, targety, targetv, targeta);
        yv
    });

    let plotter_movement = plotter_control.map(|yv| format!("XM,{},{},{}", t, t, yv));


    let ctrl_c = tokio_signal::ctrl_c(&handle).flatten_stream();

    let disabler = ctrl_c.take(1).map(|()| "SP,0\nEM,0,0".into());


    let enabler = stream::once(Ok(
        "EM,2,2\nXM,500,0,0\nXM,500,0,0\nXM,500,0,0\nSP,1\nXM,100,0,0".into(),
    ));

    let mainloop = enabler
        .chain(andselect::new(plotter_movement, disabler))
        .forward(plotter_writer);

    core.run(mainloop).unwrap();
}
