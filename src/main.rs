use std::{
    io,
    net::UdpSocket,
    sync::{Arc, RwLock},
    thread,
};

use anyhow::Result;
use jack::{Client, ClientOptions};
use std::fs::File;
use std::path::Path;

/// Should be enough,See https://osc-dev.create.ucsb.narkive.com/TyotlluU/osc-udp-packet-sizes-for-interoperability
/// and https://www.music.mcgill.ca/~gary/306/week9/osc.html
// const OSC_BUFFER_LEN: usize = 4096;
const OSC_BUFFER_LEN: usize = rosc::decoder::MTU;
const OSC_PORT: &str = "34254";
const GRAIN_BUFFER_LEN: usize = 10 * 48000; // 10s

struct Grain {
    r_buffer: [f32; GRAIN_BUFFER_LEN],
    l_buffer: [f32; GRAIN_BUFFER_LEN],
}

struct MeTest {
    status: bool,
}

fn main() -> Result<()> {
    //UDP connection for controls
    let udp_socket = UdpSocket::bind(format!("127.0.0.1:{}", OSC_PORT))?;
    let (jclient, _) = Client::new("tadeusz_jack", ClientOptions::NO_START_SERVER)?;

    //Load audio file into Grain buffer

    // let mut inp_file = File::open(Path::new("Plate1.wav"))?;
    // let (header, data) = wav::read(&mut inp_file)?;

    // Register ports. They will be used in a callback that will be
    // called when new data is available.
    let in_a = jclient
        .register_port("tadeusz_in_l", jack::AudioIn::default())
        .unwrap();
    let in_b = jclient
        .register_port("tadeusz_in_r", jack::AudioIn::default())
        .unwrap();
    let mut out_a = jclient
        .register_port("tadeusz_out_l", jack::AudioOut::default())
        .unwrap();
    let mut out_b = jclient
        .register_port("tadeusz_out_r", jack::AudioOut::default())
        .unwrap();

    let me_test = MeTest { status: false };
    let test_arc = Arc::new(RwLock::new(me_test));
    let test_clone = test_arc.clone();

    let jack_process = move |_: &jack::Client, ps: &jack::ProcessScope| -> jack::Control {
        let out_a_p = out_a.as_mut_slice(ps);
        let out_b_p = out_b.as_mut_slice(ps);
        let mut in_a_p = in_a.as_slice(ps).to_owned();
        let mut in_b_p = in_b.as_slice(ps).to_owned();

        let a = test_clone.read().unwrap();

        if !a.status {
            in_a_p = vec![0f32; out_a_p.len()];
            in_b_p = vec![0f32; out_b_p.len()];
        }
        drop(a);
        out_a_p.clone_from_slice(&in_a_p);
        out_b_p.clone_from_slice(&in_b_p);
        jack::Control::Continue
    };

    let process = jack::ClosureProcessHandler::new(jack_process);

    // Activate the client, which starts the processing.
    let active_client = jclient.activate_async((), process).unwrap();

    let test_clone = test_arc.clone();
    let osc_process = move || {
        let mut rec_buffer = [0; OSC_BUFFER_LEN];
        loop {
            match udp_socket.recv(&mut rec_buffer) {
                Ok(received) => {
                    // println!("received {} bytes {:?}", received, &rec_buffer[..received]);
                    let (_, packet) =
                        if let Ok(v) = rosc::decoder::decode_udp(&rec_buffer[..received]) {
                            v
                        } else {
                            println!("OSC message could not be decoded.");
                            continue;
                        };
                    match packet {
                        rosc::OscPacket::Message(msg) => {
                            println!("Received osc msg {:?}", msg);
                            if msg.addr == "/tadeusz" {
                                let mut a = test_clone.write().unwrap();
                                if let Some(status) = msg.args[0].to_owned().int() {
                                    (*a).status = status == 1;
                                    println!("Status set to {:?}", status == 1);
                                } else {
                                    println!("OSC message arg is unrecognized.");
                                }
                                drop(a);
                            }
                        }
                        rosc::OscPacket::Bundle(_) => unimplemented!(),
                    }
                }
                Err(e) => println!("recv function failed: {:?}", e),
            }
        }
    };

    let osc_handler = thread::spawn(osc_process);

    // Wait for user input to quit
    println!("Press enter/return to quit...");
    let mut user_input = String::new();
    io::stdin().read_line(&mut user_input).ok();

    active_client.deactivate().unwrap();
    let osc_res = osc_handler.join();
    println!("OSC shutdown: {:?}", osc_res);

    Ok(())
}
