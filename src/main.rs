use std::{
    cmp::min,
    io,
    net::UdpSocket,
    sync::{Arc, RwLock},
    thread,
};

use anyhow::Result;
use jack::{Client, ClientOptions};
use rosc::OscMessage;
use std::fs::File;
use std::path::Path;

/// Should be enough,See https://osc-dev.create.ucsb.narkive.com/TyotlluU/osc-udp-packet-sizes-for-interoperability
/// and https://www.music.mcgill.ca/~gary/306/week9/osc.html
// const OSC_BUFFER_LEN: usize = 4096;
const OSC_BUFFER_LEN: usize = rosc::decoder::MTU;
const OSC_PORT: &str = "34254";
const SAMPLE_BUFFER_MAX_LEN: usize = 10 * 48000; // 10s

struct SampleBuffer {
    len: usize,
    head: usize,
    r_buffer: Vec<f32>, // of length SAMPLE_BUFFER_MAX_LEN
    l_buffer: Vec<f32>,
}

struct Params {
    status: bool,
}

fn main() -> Result<()> {
    //Load audio file into sample buffer
    // TODO compare input file bitrate with jack server
    let mut inp_file = File::open(Path::new("Plate1.wav"))?;
    let (_, data) = wav::read(&mut inp_file)?;
    let bits: Vec<f32> = data
        .as_sixteen()
        .unwrap()
        .iter()
        .map(|&x| (x as f32) / 32768f32)
        .collect();
    // TODO wheres the stereo??
    let buffer_len = min(SAMPLE_BUFFER_MAX_LEN, bits.len());
    let mut b = SampleBuffer {
        len: buffer_len,
        head: 0,
        r_buffer: bits[0..buffer_len].to_vec(),
        l_buffer: bits[0..buffer_len].to_vec(),
    };

    // Create the shared parameters instance
    let params = Params { status: false };
    let params_arc = Arc::new(RwLock::new(params));

    // Set up jack ports
    let (jclient, _) = Client::new("tadeusz_jack", ClientOptions::NO_START_SERVER)?;
    // let in_l_port = jclient
    //     .register_port("tadeusz_in_l", jack::AudioIn::default())
    //     .unwrap();
    // let in_r_port = jclient
    //     .register_port("tadeusz_in_r", jack::AudioIn::default())
    //     .unwrap();
    let mut out_l_port = jclient
        .register_port("tadeusz_out_l", jack::AudioOut::default())
        .unwrap();
    let mut out_r_port = jclient
        .register_port("tadeusz_out_r", jack::AudioOut::default())
        .unwrap();

    // Define the Jack process (to refactor)
    let params_ref = params_arc.clone();
    let jack_process = move |_: &jack::Client, ps: &jack::ProcessScope| -> jack::Control {
        let out_l_buff = out_l_port.as_mut_slice(ps);
        let out_r_buff = out_r_port.as_mut_slice(ps);
        // let mut in_a_p = in_a.as_slice(ps).to_owned();
        // let mut in_b_p = in_b.as_slice(ps).to_owned();

        let params_read = params_ref.read().unwrap();

        let new_head = (b.head + out_l_buff.len()) % b.len;
        if b.head + out_l_buff.len() > b.len {
            // Wrapping around the sample buffer
            let (h, t) = out_l_buff.split_at_mut(b.len - b.head);
            h.copy_from_slice(&b.l_buffer[b.head..b.len]);
            t.copy_from_slice(&b.l_buffer[0..new_head]);

            let (h, t) = out_r_buff.split_at_mut(b.len - b.head);
            h.copy_from_slice(&b.r_buffer[b.head..b.len]);
            t.copy_from_slice(&b.r_buffer[0..new_head]);
        } else {
            out_l_buff.copy_from_slice(&b.l_buffer[b.head..b.head + out_l_buff.len()]);
            out_r_buff.copy_from_slice(&b.r_buffer[b.head..b.head + out_r_buff.len()]);
        };
        b.head = new_head;

        if !params_read.status {
            //             let null = vec![0f32; out_l_buff.len()];
            //                       output_r = &null;
            //   output_l = &null.clone();
        }

        jack::Control::Continue
    };

    // Start the Jack thread
    let process = jack::ClosureProcessHandler::new(jack_process);
    let active_client = jclient.activate_async((), process).unwrap();

    // Start the OSC listening thread
    let udp_socket = UdpSocket::bind(format!("127.0.0.1:{}", OSC_PORT))?;
    let osc_process = osc_process_closure(udp_socket, params_arc);
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

fn osc_handling(osc_msg: OscMessage, params_ref: &Arc<RwLock<Params>>) {
    if osc_msg.addr == "/tadeusz" {
        let mut params_mut = params_ref.write().unwrap();
        if let Some(status) = osc_msg.args[0].to_owned().int() {
            (*params_mut).status = status == 1;
            println!("Status set to {:?}", status == 1);
        } else {
            println!("OSC message arg is unrecognized.");
        }
    }
}

/// Returns a closure that runs the main osc receiving loop
fn osc_process_closure(
    udp_socket: UdpSocket,
    params_ref: Arc<RwLock<Params>>,
) -> impl FnOnce() -> Result<()> {
    move || {
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
                            osc_handling(msg, &params_ref);
                        }
                        rosc::OscPacket::Bundle(_) => unimplemented!(),
                    }
                }
                Err(e) => println!("recv function failed: {:?}", e),
            }
        }
    }
}
