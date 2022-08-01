use anyhow::Result;
use jack::{Client, ClientOptions};
use rosc::OscMessage;
use std::fs::File;
use std::path::Path;
use std::{
    cmp::min,
    io,
    net::UdpSocket,
    sync::{Arc, RwLock},
    thread,
};

/// Should be enough,See https://osc-dev.create.ucsb.narkive.com/TyotlluU/osc-udp-packet-sizes-for-interoperability
/// and https://www.music.mcgill.ca/~gary/306/week9/osc.html
// const OSC_BUFFER_LEN: usize = 4096;
const OSC_BUFFER_LEN: usize = rosc::decoder::MTU;
const OSC_PORT: &str = "34254";
const SAMPLE_BUFFER_MAX_LEN: usize = 10 * 48000; // 10s

// Write: -, Read: osc+jack
struct SampleBuffer {
    len: usize,
    r_buffer: Vec<f32>, // of length SAMPLE_BUFFER_MAX_LEN
    l_buffer: Vec<f32>,
}

#[derive(Clone)]
struct GrainParams {
    // On or off
    status: bool,
    /// Mark the params as recently changed
    updated: bool,
    len: usize,
    /// Sample index to start from
    start: usize,
    /// Cycles per second, Hz
    speed: f32,
}

#[derive(Clone)]
struct Params {
    // Write: jack process, Read: -
    grain_head: usize,
    // Write: osc process, Read: Jack process
    grain: Arc<RwLock<GrainParams>>,
}

fn main() -> Result<()> {
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

    //Load audio file into sample buffer
    let mut inp_file = File::open(Path::new("Plate1.wav"))?;
    let (header, data) = wav::read(&mut inp_file)?;
    let sample_rate = jclient.sample_rate();
    if sample_rate != header.sampling_rate as usize {
        println!(
            "Sample rate of file ({}) does not match the one from Jack ({})",
            header.sampling_rate, sample_rate
        );
        // Err(Error::msg(
        //     "Sample rate of file does not match the one from Jack",
        // ))?;
    }

    let bits: Vec<f32> = data
        .as_sixteen()
        .unwrap()
        .iter()
        .map(|&x| (x as f32) / 32768f32)
        .collect();
    // TODO wheres the stereo??
    let buffer_len = min(SAMPLE_BUFFER_MAX_LEN, bits.len());
    let buffer = SampleBuffer {
        len: buffer_len,
        r_buffer: bits[0..buffer_len].to_vec(),
        l_buffer: bits[0..buffer_len].to_vec(),
    };

    // Create the shared parameters instance
    let grain_params_arc = Arc::new(RwLock::new(GrainParams {
        status: true,
        updated: false,
        start: 0,
        len: buffer.len,
        speed: 1.,
    }));
    let params_arc = Params {
        grain_head: 0,
        grain: grain_params_arc,
    };

    // Define the Jack process (to refactor)
    let mut params_ref = params_arc.clone();
    let b_arc = Arc::new(buffer);
    let b_ref = b_arc.clone();
    let jack_process = move |_: &jack::Client, ps: &jack::ProcessScope| -> jack::Control {
        let out_l_buff = out_l_port.as_mut_slice(ps);
        let out_r_buff = out_r_port.as_mut_slice(ps);
        // let mut in_a_p = in_a.as_slice(ps).to_owned();
        // let mut in_b_p = in_b.as_slice(ps).to_owned();

        let grain_params_read = params_ref.grain.read().unwrap();

        // Relying on buffer L R being same length
        let out_buff_len = out_l_buff.len();
        let grain_len = grain_params_read.len;
        for i in 0..out_buff_len {
            out_l_buff[i] = b_ref.l_buffer
                [(grain_params_read.start + (params_ref.grain_head + i) % grain_len) % b_ref.len];
            out_r_buff[i] = b_ref.r_buffer
                [(grain_params_read.start + (params_ref.grain_head + i) % grain_len) % b_ref.len];
        }
        params_ref.grain_head = (params_ref.grain_head + out_buff_len) % grain_len;

        if !grain_params_read.status {
            out_l_buff.fill(0.);
            out_r_buff.fill(0.);
        }

        jack::Control::Continue
    };

    // Start the Jack thread
    let process = jack::ClosureProcessHandler::new(jack_process);
    let active_client = jclient.activate_async((), process).unwrap();

    // Start the OSC listening thread
    let udp_socket = UdpSocket::bind(format!("127.0.0.1:{}", OSC_PORT))?;
    let osc_process = osc_process_closure(udp_socket, params_arc, b_arc);
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

// TODO should not be able to fail, remove unwraps and return error obj to be catched & printed
fn osc_handling(osc_msg: &OscMessage, params: &Params, buffer: &SampleBuffer) {
    match osc_msg.addr.as_str() {
        "/tadeusz/status" => {
            if let Some(status) = osc_msg.args[0].to_owned().int() {
                let mut grain_params_mut = params.grain.write().unwrap();
                grain_params_mut.status = status == 1;
                println!("Status set to {:?}", status == 1);
            } else {
                println!("OSC message argument is of wrong type.");
            }
        }
        "/tadeusz/params" => {
            let mut grain_params_mut = params.grain.write().unwrap();
            if let Some(start) = osc_msg.args[0].to_owned().int()
                && let Some(len) = osc_msg.args[1].to_owned().int() {
                    if len > 0 {
                        grain_params_mut.start = min(start as usize, buffer.len);
                        grain_params_mut.len = min(len as usize, buffer.len);
                        println!("Grain start set to {:?}", grain_params_mut.start);
                        println!("Grain len set to {:?}", len);
                    } else {
                        println!("OSC len message argument cannot be 0.");
                    }
                }   else {
                println!("OSC message argument is of wrong type.");
            }
        }
        _ => println!("OSC message routing is unrecognized."),
    }
}

/// Returns a closure that runs the main osc receiving loop
fn osc_process_closure<'a, 'b>(
    udp_socket: UdpSocket,
    params_ref: Params,
    buffer: Arc<SampleBuffer>,
) -> impl FnOnce() -> Result<()> + 'b
where
    'a: 'b,
{
    move || {
        let mut rec_buffer = [0; OSC_BUFFER_LEN];
        loop {
            match udp_socket.recv(&mut rec_buffer) {
                Ok(received) => {
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
                            osc_handling(&msg, &params_ref, &buffer);
                        }
                        rosc::OscPacket::Bundle(_) => unimplemented!(),
                    }
                }
                Err(e) => println!("recv function failed: {:?}", e),
            }
        }
    }
}
