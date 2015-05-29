use std::collections::BTreeMap;
use std::io::{self, Read, Write, BufRead, Lines};
use std::convert::From;
use std::fmt::Arguments;
use std::net::{TcpStream, ToSocketAddrs};

use bufstream::BufStream;
use version::Version;
use error::{ProtoError, Error, Result};
use reply::Reply;
use status::Status;
use stats::Stats;
use replaygain::ReplayGain;
use song::{Song, Id};
use output::Output;
use playlist::Playlist;
use plugin::Plugin;
use message::{Channel, Message};
use search::Query;

use traits::*;

// Iterator {{{
struct Pairs<I>(I);

impl<I> Iterator for Pairs<I> where I: Iterator<Item=io::Result<String>> {
    type Item = Result<(String, String)>;
    fn next(&mut self) -> Option<Result<(String, String)>> {
        let reply: Option<Result<Reply>> = self.0.next().map(|v| v.map_err(Error::Io).and_then(|s| s.parse::<Reply>().map_err(Error::Parse)));
        match reply {
            Some(Ok(Reply::Pair(a, b))) => Some(Ok((a, b))),
            None | Some(Ok(Reply::Ok)) => None,
            Some(Ok(Reply::Ack(e))) => Some(Err(Error::Server(e))),
            Some(Err(e)) => Some(Err(e)),
        }
    }
}

struct Maps<'a, I: 'a> {
    pairs: &'a mut Pairs<I>,
    sep: &'a str,
    value: Option<String>,
    done: bool
}

impl<'a, I> Iterator for Maps<'a, I> where I: Iterator<Item=io::Result<String>> {
    type Item = Result<BTreeMap<String, String>>;
    fn next(&mut self) -> Option<Result<BTreeMap<String, String>>> {
        if self.done {
            return None;
        }

        let mut map = BTreeMap::new();

        if let Some(b) = self.value.take() {
            map.insert(self.sep.to_owned(), b);
        }

        loop {
            match self.pairs.next() {
                Some(Ok((a, b))) => {
                    if &*a == self.sep {
                        self.value = Some(b);
                        break;
                    } else {
                        map.insert(a, b);
                    }
                },
                Some(Err(e)) => return Some(Err(e)),
                None => {
                    self.done = true;
                    break;
                }
            }
        }

        Some(Ok(map))
    }
}

impl<I> Pairs<I> where I: Iterator<Item=io::Result<String>> {
    fn split<'a, 'b: 'a>(&'a mut self, f: &'b str) -> Maps<'a, I> {
        let mut maps = Maps {
            pairs: self,
            sep: f,
            value: None,
            done: false,
        };
        maps.next(); // swallow first separator
        maps
    }
}
// }}}

// Client {{{
#[derive(Debug)]
pub struct Client<S=TcpStream> where S: Read+Write {
    socket: BufStream<S>,
    pub version: Version
}

impl Default for Client<TcpStream> {
    fn default() -> Client<TcpStream> {
        Client::<TcpStream>::connect("127.0.0.1:6600").unwrap()
    }
}

impl<S: Read+Write> Client<S> {
    // Constructors {{{
    pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<Client<TcpStream>> {
        TcpStream::connect(addr).map_err(Error::Io).and_then(Client::new)
    }

    pub fn new(socket: S) -> Result<Client<S>> {
        let mut socket = BufStream::new(socket);

        let mut banner = String::new();
        try!(socket.read_line(&mut banner));

        if !banner.starts_with("OK MPD ") {
            return Err(From::from(ProtoError::BadBanner));
        }

        let version = try!(banner[7..].trim().parse::<Version>());

        Ok(Client {
            socket: socket,
            version: version
        })
    }
    // }}}

    // Playback options & status {{{
    pub fn status(&mut self) -> Result<Status> {
        self.run_command("status")
            .and_then(|_| self.read_map())
            .and_then(Status::from_map)
    }

    pub fn stats(&mut self) -> Result<Stats> {
        self.run_command("stats")
            .and_then(|_| self.read_map())
            .and_then(Stats::from_map)
    }

    pub fn clearerror(&mut self) -> Result<()> {
        self.run_command("clearerror")
            .and_then(|_| self.expect_ok())
    }

    pub fn volume(&mut self, volume: i8) -> Result<()> {
        self.run_command_fmt(format_args!("setvol {}", volume))
            .and_then(|_| self.expect_ok())
    }

    pub fn repeat(&mut self, value: bool) -> Result<()> {
        self.run_command_fmt(format_args!("repeat {}", value as u8))
            .and_then(|_| self.expect_ok())
    }

    pub fn random(&mut self, value: bool) -> Result<()> {
        self.run_command_fmt(format_args!("random {}", value as u8))
            .and_then(|_| self.expect_ok())
    }

    pub fn single(&mut self, value: bool) -> Result<()> {
        self.run_command_fmt(format_args!("single {}", value as u8))
            .and_then(|_| self.expect_ok())
    }

    pub fn consume(&mut self, value: bool) -> Result<()> {
        self.run_command_fmt(format_args!("consume {}", value as u8))
            .and_then(|_| self.expect_ok())
    }

    pub fn crossfade(&mut self, value: u64) -> Result<()> {
        self.run_command_fmt(format_args!("crossfade {}", value))
            .and_then(|_| self.expect_ok())
    }

    pub fn mixrampdb(&mut self, value: f32) -> Result<()> {
        self.run_command_fmt(format_args!("mixrampdb {}", value))
            .and_then(|_| self.expect_ok())
    }

    pub fn mixrampdelay<T: ToSeconds>(&mut self, value: T) -> Result<()> {
        self.run_command_fmt(format_args!("mixrampdelay {}", value.to_seconds()))
            .and_then(|_| self.expect_ok())
    }

    pub fn replaygain(&mut self, gain: ReplayGain) -> Result<()> {
        self.run_command_fmt(format_args!("replay_gain_mode {}", gain))
            .and_then(|_| self.expect_ok())
    }

    pub fn get_replaygain(&mut self) -> Result<ReplayGain> {
        self.run_command("replay_gain_status")
            .and_then(|_| self.read_field("replay_gain_mode"))
            .and_then(|v| self.expect_ok()
                      .and_then(|_| v.parse().map_err(From::from)))
    }
    // }}}

    // Playback control {{{
    pub fn play(&mut self) -> Result<()> {
        self.run_command("play").and_then(|_| self.expect_ok())
    }

    pub fn switch<T: ToQueuePlace>(&mut self, place: T) -> Result<()> {
        self.run_command_fmt(format_args!("play{} {}", if T::is_id() { "id" } else { "" }, place.to_place()))
            .and_then(|_| self.expect_ok())
    }

    pub fn next(&mut self) -> Result<()> {
        self.run_command("next")
            .and_then(|_| self.expect_ok())
    }

    pub fn prev(&mut self) -> Result<()> {
        self.run_command("previous")
            .and_then(|_| self.expect_ok())
    }

    pub fn stop(&mut self) -> Result<()> {
        self.run_command("stop")
            .and_then(|_| self.expect_ok())
    }

    pub fn pause(&mut self, value: bool) -> Result<()> {
        self.run_command_fmt(format_args!("pause {}", value as u8))
            .and_then(|_| self.expect_ok())
    }

    pub fn seek<T: ToSeconds, P: ToQueuePlace>(&mut self, place: P, pos: T) -> Result<()> {
        self.run_command_fmt(format_args!("seek{} {} {}", if P::is_id() { "id" } else { "" }, place.to_place(), pos.to_seconds()))
            .and_then(|_| self.expect_ok())
    }

    pub fn rewind<T: ToSeconds>(&mut self, pos: T) -> Result<()> {
        self.run_command_fmt(format_args!("seekcur {}", pos.to_seconds()))
            .and_then(|_| self.expect_ok())
    }
    // }}}

    // Queue control {{{
    pub fn queue<T: ToQueueRangeOrPlace>(&mut self, pos: T) -> Result<Vec<Song>> {
        self.run_command_fmt(format_args!("playlist{} {}", if T::is_id() { "id" } else { "info" }, pos.to_range()))
            .and_then(|_| self.read_pairs().split("file").map(|v| v.and_then(Song::from_map)).collect())
    }

    pub fn currentsong(&mut self) -> Result<Option<Song>> {
        self.run_command("currentsong")
            .and_then(|_| self.read_map())
            .and_then(|m| if m.is_empty() {
                Ok(None)
            } else {
                Song::from_map(m).map(Some)
            })
    }

    pub fn clear(&mut self) -> Result<()> {
        self.run_command("clear")
            .and_then(|_| self.expect_ok())
    }

    pub fn changes(&mut self, version: u32) -> Result<Vec<Song>> {
        self.run_command_fmt(format_args!("plchanges {}", version))
            .and_then(|_| self.read_pairs().split("file").map(|v| v.and_then(Song::from_map)).collect())
    }

    pub fn append(&mut self, path: &str) -> Result<Id> {
        self.run_command_fmt(format_args!("addid \"{}\"", path))
            .and_then(|_| self.read_field("Id"))
            .and_then(|v| self.expect_ok()
                      .and_then(|_| v.parse().map_err(From::from).map(Id)))
    }

    pub fn insert(&mut self, path: &str, pos: usize) -> Result<usize> {
        self.run_command_fmt(format_args!("addid \"{}\" {}", path, pos))
            .and_then(|_| self.read_field("Id"))
            .and_then(|v| self.expect_ok()
                      .and_then(|_| v.parse().map_err(From::from)))
    }

    pub fn delete<T: ToQueueRangeOrPlace>(&mut self, pos: T) -> Result<()> {
            self.run_command_fmt(format_args!("delete{} {}", if T::is_id() { "id" } else { "" }, pos.to_range()))
                .and_then(|_| self.expect_ok())
    }

    pub fn shift<T: ToQueueRangeOrPlace>(&mut self, from: T, to: usize) -> Result<()> {
        self.run_command_fmt(format_args!("move{} {} {}", if T::is_id() { "id" } else { "" }, from.to_range(), to))
            .and_then(|_| self.expect_ok())
    }

    pub fn swap<T: ToQueuePlace>(&mut self, one: T, two: T) -> Result<()> {
        self.run_command_fmt(format_args!("swap{} {} {}", if T::is_id() { "id" } else { "" }, one.to_place(), two.to_place()))
            .and_then(|_| self.expect_ok())
    }

    pub fn shuffle<T: ToQueueRange>(&mut self, range: T) -> Result<()> {
        self.run_command_fmt(format_args!("shuffle {}", range.to_range()))
            .and_then(|_| self.expect_ok())
    }

    pub fn priority<T: ToQueueRangeOrPlace>(&mut self, pos: T, prio: u8) -> Result<()> {
        self.run_command_fmt(format_args!("prio{} {} {}", if T::is_id() { "id" } else { "" }, prio, pos.to_range()))
            .and_then(|_| self.expect_ok())
    }
    // }}}

    // Connection settings {{{
    pub fn ping(&mut self) -> Result<()> {
        self.run_command("ping").and_then(|_| self.expect_ok())
    }

    pub fn close(&mut self) -> Result<()> {
        self.run_command("close").and_then(|_| self.expect_ok())
    }

    pub fn kill(&mut self) -> Result<()> {
        self.run_command("kill").and_then(|_| self.expect_ok())
    }

    pub fn login(&mut self, password: &str) -> Result<()> {
        self.run_command_fmt(format_args!("password \"{}\"", password)).and_then(|_| self.expect_ok())
    }
    // }}}

    // Playlist methods {{{
    pub fn playlists(&mut self) -> Result<Vec<Playlist>> {
        self.run_command("listplaylists")
            .and_then(|_| self.read_pairs().split("playlist").map(|v| v.and_then(Playlist::from_map)).collect())
    }

    pub fn pl_load<T: ToQueueRange>(&mut self, name: &str, range: T) -> Result<()> {
        self.run_command_fmt(format_args!("load \"{}\" {}", name, range.to_range()))
            .and_then(|_| self.expect_ok())
    }
    pub fn pl_clear(&mut self, name: &str) -> Result<()> {
        self.run_command_fmt(format_args!("playlistclear \"{}\"", name))
            .and_then(|_| self.expect_ok())
    }
    pub fn pl_remove(&mut self, name: &str) -> Result<()> {
        self.run_command_fmt(format_args!("rm \"{}\"", name))
            .and_then(|_| self.expect_ok())
    }
    pub fn pl_save(&mut self, name: &str) -> Result<()> {
        self.run_command_fmt(format_args!("save \"{}\"", name))
            .and_then(|_| self.expect_ok())
    }
    pub fn pl_rename(&mut self, name: &str, newname: &str) -> Result<()> {
        self.run_command_fmt(format_args!("rename \"{}\" \"{}\"", name, newname))
            .and_then(|_| self.expect_ok())
    }
    pub fn pl_songs(&mut self, name: &str) -> Result<Vec<Song>> {
        self.run_command_fmt(format_args!("listplaylistinfo \"{}\"", name))
            .and_then(|_| self.read_pairs().split("file").map(|v| v.and_then(Song::from_map)).collect())
    }
    pub fn pl_append(&mut self, name: &str, path: &str) -> Result<()> {
        self.run_command_fmt(format_args!("playlistadd \"{}\" \"{}\"", name, path))
            .and_then(|_| self.expect_ok())
    }
    pub fn pl_delete(&mut self, name: &str, pos: u32) -> Result<()> {
        self.run_command_fmt(format_args!("playlistdelete \"{}\" {}", name, pos))
            .and_then(|_| self.expect_ok())
    }
    pub fn pl_shift(&mut self, name: &str, from: u32, to: u32) -> Result<()> {
        self.run_command_fmt(format_args!("playlistmove \"{}\" {} {}", name, from, to))
            .and_then(|_| self.expect_ok())
    }
    // }}}

    // Database methods {{{
    pub fn rescan(&mut self) -> Result<u32> {
        self.run_command("rescan")
            .and_then(|_| self.read_field("updating_db"))
            .and_then(|v| self.expect_ok()
                      .and_then(|_| v.parse().map_err(From::from)))
    }

    pub fn update(&mut self) -> Result<u32> {
        self.run_command("update")
            .and_then(|_| self.read_field("updating_db"))
            .and_then(|v| self.expect_ok()
                      .and_then(|_| v.parse().map_err(From::from)))
    }
    // }}}

    // Database search {{{
    pub fn search(&mut self, query: Query) -> Result<Vec<Song>> {
        self.run_command_fmt(format_args!("search {}", query))
            .and_then(|_| self.read_pairs().split("file").map(|v| v.and_then(Song::from_map)).collect())
    }
    // }}}

    // Output methods {{{
    pub fn outputs(&mut self) -> Result<Vec<Output>> {
        self.run_command("outputs")
            .and_then(|_| self.read_pairs().split("outputid").map(|v| v.and_then(Output::from_map)).collect())
    }

    pub fn out_disable<T: ToOutputId>(&mut self, id: T) -> Result<()> {
        self.run_command_fmt(format_args!("disableoutput {}", id.to_output_id()))
            .and_then(|_| self.expect_ok())
    }

    pub fn out_enable<T: ToOutputId>(&mut self, id: T) -> Result<()> {
        self.run_command_fmt(format_args!("enableoutput {}", id.to_output_id()))
            .and_then(|_| self.expect_ok())
    }

    pub fn out_toggle<T: ToOutputId>(&mut self, id: T) -> Result<()> {
        self.run_command_fmt(format_args!("toggleoutput {}", id.to_output_id()))
            .and_then(|_| self.expect_ok())
    }
    // }}}

    // Reflection methods {{{
    pub fn music_directory(&mut self) -> Result<String> {
        self.run_command("config")
            .and_then(|_| self.read_field("music_directory"))
            .and_then(|d| self.expect_ok().map(|_| d))
    }

    pub fn commands(&mut self) -> Result<Vec<String>> {
        self.run_command("commands")
            .and_then(|_| self.read_pairs()
                      .filter(|r| r.as_ref()
                              .map(|&(ref a, _)| *a == "command").unwrap_or(true))
                      .map(|r| r.map(|(_, b)| b))
                      .collect())
    }

    pub fn notcommands(&mut self) -> Result<Vec<String>> {
        self.run_command("notcommands")
            .and_then(|_| self.read_pairs()
                      .filter(|r| r.as_ref()
                              .map(|&(ref a, _)| *a == "command").unwrap_or(true))
                      .map(|r| r.map(|(_, b)| b))
                      .collect())
    }

    pub fn urlhandlers(&mut self) -> Result<Vec<String>> {
        self.run_command("urlhandlers")
            .and_then(|_| self.read_pairs()
                      .filter(|r| r.as_ref()
                              .map(|&(ref a, _)| *a == "handler").unwrap_or(true))
                      .map(|r| r.map(|(_, b)| b))
                      .collect())
    }

    pub fn tagtypes(&mut self) -> Result<Vec<String>> {
        self.run_command("tagtypes")
            .and_then(|_| self.read_pairs()
                      .filter(|r| r.as_ref()
                              .map(|&(ref a, _)| *a == "tagtype").unwrap_or(true))
                      .map(|r| r.map(|(_, b)| b))
                      .collect())
    }

    pub fn decoders(&mut self) -> Result<Vec<Plugin>> {
        try!(self.run_command("decoders"));

        let mut result = Vec::new();
        let mut plugin: Option<Plugin> = None;
        for reply in self.read_pairs() {
            let (a, b) = try!(reply);
            match &*a {
                "plugin" => {
                    plugin.map(|p| result.push(p));

                    plugin = Some(Plugin {
                        name: b,
                        suffixes: Vec::new(),
                        mime_types: Vec::new()
                    });
                },
                "mime_type" => { plugin.as_mut().map(|p| p.mime_types.push(b)); }
                "suffix" => { plugin.as_mut().map(|p| p.suffixes.push(b)); }
                _ => unreachable!()
            }
        }
        plugin.map(|p| result.push(p));
        Ok(result)
    }
    // }}}

    // Messaging {{{
    pub fn channels(&mut self) -> Result<Vec<Channel>> {
        self.run_command("channels")
            .and_then(|_| self.read_pairs()
                      .filter(|r| r.as_ref()
                              .map(|&(ref a, _)| *a == "channel").unwrap_or(true))
                      .map(|r| r.map(|(_, b)| unsafe { Channel::new_unchecked(b) }))
                      .collect())
    }

    pub fn readmessages(&mut self) -> Result<Vec<Message>> {
        self.run_command("readmessages")
            .and_then(|_| self.read_pairs().split("channel").map(|v| v.and_then(Message::from_map)).collect())
    }

    pub fn sendmessage(&mut self, channel: Channel, message: &str) -> Result<()> {
        self.run_command_fmt(format_args!("sendmessage {} \"{}\"", channel, message))
            .and_then(|_| self.expect_ok())
    }

    pub fn subscribe(&mut self, channel: Channel) -> Result<()> {
        self.run_command_fmt(format_args!("subscribe {}", channel))
            .and_then(|_| self.expect_ok())
    }

    pub fn unsubscribe(&mut self, channel: Channel) -> Result<()> {
        self.run_command_fmt(format_args!("unsubscribe {}", channel))
            .and_then(|_| self.expect_ok())
    }
    // }}}

    // Helper methods {{{
    fn read_line(&mut self) -> Result<String> {
        let mut buf = String::new();
        try!(self.socket.read_line(&mut buf));
        if buf.ends_with("\n") {
            buf.pop();
        }
        Ok(buf)
    }

    fn read_pairs(&mut self) -> Pairs<Lines<&mut BufStream<S>>> {
        Pairs((&mut self.socket).lines())
    }

    fn read_map(&mut self) -> Result<BTreeMap<String, String>> {
        self.read_pairs().collect()
    }

    fn run_command(&mut self, command: &str) -> Result<()> {
        self.socket.write_all(command.as_bytes())
            .and_then(|_| self.socket.write(&[0x0a]))
            .and_then(|_| self.socket.flush())
            .map_err(From::from)
    }

    fn run_command_fmt(&mut self, command: Arguments) -> Result<()> {
        self.socket.write_fmt(command)
            .and_then(|_| self.socket.write(&[0x0a]))
            .and_then(|_| self.socket.flush())
            .map_err(From::from)
    }

    fn expect_ok(&mut self) -> Result<()> {
        let line = try!(self.read_line());

        match line.parse::<Reply>() {
            Ok(Reply::Ok) => Ok(()),
            Ok(Reply::Ack(e)) => Err(Error::Server(e)),
            Ok(_) => Err(Error::Proto(ProtoError::NotOk)),
            Err(e) => Err(From::from(e)),
        }
    }

    fn read_pair(&mut self) -> Result<(String, String)> {
        let line = try!(self.read_line());

        match line.parse::<Reply>() {
            Ok(Reply::Pair(a, b)) => Ok((a, b)),
            Ok(Reply::Ok) => Err(Error::Proto(ProtoError::NotPair)),
            Ok(Reply::Ack(e)) => Err(Error::Server(e)),
            Err(e) => Err(Error::Parse(e)),
        }
    }

    fn read_field(&mut self, field: &'static str) -> Result<String> {
        let (a, b) = try!(self.read_pair());
        if &*a == field {
            Ok(b)
        } else {
            Err(Error::Proto(ProtoError::NoField(field)))
        }
    }
    // }}}
}

// }}}

