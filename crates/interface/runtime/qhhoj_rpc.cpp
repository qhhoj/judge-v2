// Split signature runtime: codec bodies, frames over fds 3 (from the peer) and 4 (to the peer), the jury's
// charge record, and how each side ends. Never includes the problem header. C++03.
//
// Ending (each mirrors what one process would do):
// - the jury exits (main returns, exit anywhere): the contestant reads EOF and _exits 0;
// - the contestant calls exit(): its atexit sends EXITING and the jury calls exit(0), flushing its output;
// - the contestant _exits, crashes or is killed: the jury reads EOF and _exits 0 without flushing; the judge
//   takes the contestant's own verdict;
// - a bad frame: the jury writes FAULT_FILE and exits (the judge's "protocol violation"); the contestant exits
//   BAD_FRAME_EXIT.
//
// Cost: a call is one frame each way, one write() and usually one read() per frame, and no allocation once the
// buffers have grown. Before each frame the jury compares every remembered `const&` object with what it last
// sent, in place (one memcmp for a vector of scalars), and sends only those that changed: that compare is
// O(object size) per frame, charged to the contestant (it exists only for the references the contestant holds).
#include "qhhoj_rpc.hpp"

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

#include <map>

namespace qhhoj {
namespace {

const int FROM_PEER = 3;
const int TO_PEER = 4;
const int BAD_FRAME_EXIT = 117;
const int PROTOCOL_EXIT = 118;
const char FAULT_FILE[] = "/box/.qhhoj-fault";
// The jury's charge record (two native u64: nanoseconds of jury CPU charged so far, and the jury's process CPU
// clock when the callback now running began, 0 when none). The judge reads it after the run.
const char CHARGE_FILE[] = "/box/.qhhoj-charge";
// A quarter of the jury's 1 GiB floor: a hostile length is refused before anything is allocated for it.
const uint32_t MAX_MESSAGE = 256u << 20;
const Size HEADER_BYTES = 8;

enum Side { JURY, CONTESTANT };
Side side = JURY;
Dispatch contestant_serve = 0;
bool exiting = false;

// The jury's remembered `const&` results: per function, each object it returned (its id is its index).
struct Remembered {
  const void* object;
  Encoder encoder;
  Bytes sent;
};
std::vector<std::vector<Remembered> > remembered;
std::vector<std::map<const void*, uint32_t> > remembered_ids;
bool remembers_any = false;

// The contestant's copies, per function and id.
struct Tracked {
  Decoder decoder;
  std::vector<void*> objects;
  Tracked() : decoder(0) {}
};
std::vector<Tracked> tracked;

// Reused buffers: the outgoing frame (header then payload), the CACHE frames that go before it, the payload
// of the frame just read, and the jury's scratch encoding of a remembered object.
Bytes out_frame;
Bytes cache_frames;
Bytes in_payload;
Bytes scratch;

struct ChargePage {
  uint64_t charged_ns;
  uint64_t open_since_ns;
};
volatile ChargePage* charge_page = 0;
bool charge_opened = false;
int serving_depth = 0;

// Buffered reads of fd 3: one read() usually brings a whole frame.
unsigned char in_buffer[1 << 16];
Size in_begin = 0;
Size in_end = 0;

bool fill() {
  for (;;) {
    ssize_t got = read(FROM_PEER, in_buffer, sizeof in_buffer);
    if (got > 0) {
      in_begin = 0;
      in_end = static_cast<Size>(got);
      return true;
    }
    if (got == 0 || errno != EINTR) return false;
  }
}

bool read_exact(unsigned char* out, Size size) {
  while (size > 0) {
    if (in_begin == in_end && !fill()) return false;
    Size step = in_end - in_begin < size ? in_end - in_begin : size;
    memcpy(out, in_buffer + in_begin, step);
    in_begin += step;
    out += step;
    size -= step;
  }
  return true;
}

bool write_all(const unsigned char* data, Size size) {
  while (size > 0) {
    ssize_t put = write(TO_PEER, data, size);
    if (put < 0 && errno == EINTR) continue;
    if (put <= 0) return false;
    data += put;
    size -= static_cast<Size>(put);
  }
  return true;
}

// Reads one frame into `in_payload`. False at EOF, also in the middle of a frame (the peer is gone).
bool read_frame(uint16_t& kind, uint16_t& fn) {
  unsigned char header[HEADER_BYTES];
  if (!read_exact(header, HEADER_BYTES)) return false;
  uint32_t length;
  memcpy(&length, header, 4);
  memcpy(&kind, header + 4, 2);
  memcpy(&fn, header + 6, 2);
  if (length > MAX_MESSAGE) protocol_fault("message too large");
  in_payload.resize(length);
  return length == 0 || read_exact(&in_payload[0], length);
}

// A side that would send more than the peer accepts ends itself (SIGABRT: a runtime error).
void put_header(unsigned char* at, Size payload, uint16_t kind, uint16_t fn) {
  if (payload > MAX_MESSAGE) abort();
  uint32_t length = static_cast<uint32_t>(payload);
  memcpy(at, &length, 4);
  memcpy(at + 4, &kind, 2);
  memcpy(at + 6, &fn, 2);
}

// The contestant called exit(): flush and end the jury the way the shared process would have ended.
void jury_exit() {
  if (exiting) _exit(0);
  exiting = true;
  exit(0);
}

// EOF from the peer, or a write it can no longer read. The jury looks for an EXITING still in the pipe.
void peer_gone() __attribute__((noreturn));
void peer_gone() {
  if (side == CONTESTANT) _exit(0);
  uint16_t kind;
  uint16_t fn;
  while (read_frame(kind, fn)) {
    if (kind == KIND_EXITING) jury_exit();
  }
  _exit(0);
}

uint64_t cpu_now_ns() {
  struct timespec now;
  if (clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &now) != 0) return 0;
  return static_cast<uint64_t>(now.tv_sec) * 1000000000u + static_cast<uint64_t>(now.tv_nsec);
}

// Created by the jury in its own box on the first callback; without it nothing is recorded.
void open_charge_page() {
  charge_opened = true;
  int fd = open(CHARGE_FILE, O_RDWR | O_CREAT | O_TRUNC, 0644);
  if (fd < 0) return;
  if (ftruncate(fd, sizeof(ChargePage)) == 0) {
    void* page = mmap(0, sizeof(ChargePage), PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (page != MAP_FAILED) charge_page = static_cast<ChargePage*>(page);
  }
  close(fd);
}

// A CACHE frame for object `id` of `fn`, queued to go before the frame being sent.
void append_cache(uint16_t fn, uint32_t id, const Bytes& value) {
  Size at = cache_frames.size();
  cache_frames.resize(at + HEADER_BYTES + 4);
  put_header(&cache_frames[at], 4 + value.size(), KIND_CACHE, fn);
  memcpy(&cache_frames[at + HEADER_BYTES], &id, 4);
  cache_frames.insert(cache_frames.end(), value.begin(), value.end());
}

// Before every frame the jury sends: each remembered `const&` object that changed goes out as CACHE.
void refresh_caches() {
  cache_frames.clear();
  for (Size fn = 0; fn < remembered.size(); ++fn) {
    for (Size id = 0; id < remembered[fn].size(); ++id) {
      Remembered& entry = remembered[fn][id];
      scratch.clear();
      Writer now(scratch, entry.sent);
      entry.encoder(now, entry.object);
      if (now.unchanged()) continue;
      entry.sent.swap(scratch);
      append_cache(static_cast<uint16_t>(fn), static_cast<uint32_t>(id), entry.sent);
    }
  }
}

// Sends `out_frame` (its payload already written), after any CACHE frames the jury owes.
void send(uint16_t kind, uint16_t fn) {
  put_header(&out_frame[0], out_frame.size() - HEADER_BYTES, kind, fn);
  if (side == JURY && remembers_any) {
    // The re-check exists only because the contestant holds `const&` results: charged like a callback.
    Serving charged(fn);
    refresh_caches();
  }
  bool sent;
  if (side == JURY && !cache_frames.empty()) {
    cache_frames.insert(cache_frames.end(), out_frame.begin(), out_frame.end());
    sent = write_all(&cache_frames[0], cache_frames.size());
  } else {
    sent = write_all(&out_frame[0], out_frame.size());
  }
  if (!sent) peer_gone();
}

void receive(uint16_t& kind, uint16_t& fn) {
  if (!read_frame(kind, fn)) peer_gone();
  if (kind == KIND_EXITING && side == JURY) jury_exit();
}

Tracked& tracked_for(uint16_t fn) {
  if (tracked.size() <= fn) tracked.resize(fn + 1u);
  return tracked[fn];
}

void apply_cache(uint16_t fn) {
  if (side != CONTESTANT || fn >= tracked.size()) protocol_fault("unexpected cache frame");
  Reader in(in_payload);
  uint32_t id = in.u32();
  Tracked& entry = tracked[fn];
  if (id >= entry.objects.size()) protocol_fault("cache frame for an unknown object");
  entry.decoder(in, entry.objects[id]);
  in.end();
}

// A frame that is not the awaited RETURN: a call to serve, or a cache refresh.
void handle(uint16_t kind, uint16_t fn, Dispatch serve) {
  if (kind == KIND_CACHE) return apply_cache(fn);
  if (kind != KIND_CALL || serve == 0) protocol_fault("unexpected frame");
  Request request(fn, in_payload);
  serve(request);
}

void send_exiting() {
  if (exiting) return;
  exiting = true;
  unsigned char header[HEADER_BYTES];
  put_header(header, 0, KIND_EXITING, 0);
  write_all(header, HEADER_BYTES);
}

void ignore_sigpipe() {
  static bool done = false;
  if (done) return;
  done = true;
  signal(SIGPIPE, SIG_IGN);
}

template <class T> void encode_scalars(Writer& out, const std::vector<T>& value) {
  out.u32(static_cast<uint32_t>(value.size()));
  if (!value.empty()) out.raw(&value[0], value.size() * sizeof(T));
}

template <class T> void decode_scalars(Reader& in, std::vector<T>& value) {
  value.resize(in.count(sizeof(T)));
  if (!value.empty()) in.raw(&value[0], value.size() * sizeof(T));
}

}  // namespace

void protocol_fault(const char* what) {
  if (side == CONTESTANT) _exit(BAD_FRAME_EXIT);
  int fd = open(FAULT_FILE, O_WRONLY | O_CREAT | O_TRUNC, 0644);
  if (fd >= 0) {
    ssize_t ignored = write(fd, what, strlen(what));
    (void)ignored;
    close(fd);
  }
  _exit(PROTOCOL_EXIT);
}

bool Writer::still_matches(const unsigned char* bytes, Size size) {
  if (size == 0) return true;
  if (matched_ + size <= expected_->size() && memcmp(&(*expected_)[matched_], bytes, size) == 0) {
    matched_ += size;
    return true;
  }
  bytes_->assign(expected_->begin(), expected_->begin() + matched_);
  expected_ = 0;
  return false;
}

void Reader::raw(void* out, Size size) {
  if (size > left()) protocol_fault("payload too short");
  if (size != 0) memcpy(out, &bytes_[at_], size);
  at_ += size;
}

void encode(Writer& out, int value) { out.raw(&value, sizeof value); }
void encode(Writer& out, long long value) { out.raw(&value, sizeof value); }
void encode(Writer& out, double value) { out.raw(&value, sizeof value); }
void encode(Writer& out, bool value) {
  unsigned char byte = value ? 1 : 0;
  out.raw(&byte, 1);
}
void encode(Writer& out, const std::string& value) {
  out.u32(static_cast<uint32_t>(value.size()));
  out.raw(value.data(), value.size());
}
void encode(Writer& out, const std::vector<bool>& value) {
  out.u32(static_cast<uint32_t>(value.size()));
  Bytes bits((value.size() + 7) / 8, 0);
  for (Size i = 0; i < value.size(); ++i) {
    if (value[i]) bits[i / 8] |= static_cast<unsigned char>(1u << (i % 8));
  }
  if (!bits.empty()) out.raw(&bits[0], bits.size());
}
void encode(Writer& out, const std::vector<int>& value) { encode_scalars(out, value); }
void encode(Writer& out, const std::vector<long long>& value) { encode_scalars(out, value); }
void encode(Writer& out, const std::vector<double>& value) { encode_scalars(out, value); }

void decode(Reader& in, int& value) { in.raw(&value, sizeof value); }
void decode(Reader& in, long long& value) { in.raw(&value, sizeof value); }
void decode(Reader& in, double& value) { in.raw(&value, sizeof value); }
void decode(Reader& in, bool& value) {
  unsigned char byte;
  in.raw(&byte, 1);
  if (byte > 1) protocol_fault("bad bool");
  value = byte == 1;
}
void decode(Reader& in, std::string& value) {
  uint32_t size = in.count(1);
  value.resize(size);
  if (size != 0) in.raw(&value[0], size);
}
void decode(Reader& in, std::vector<bool>& value) {
  uint32_t size = in.u32();
  Size bytes = (static_cast<Size>(size) + 7) / 8;
  if (bytes > in.left()) protocol_fault("count past the payload");
  Bytes bits(bytes);
  if (bytes != 0) in.raw(&bits[0], bytes);
  if (size % 8 != 0 && (bits[bytes - 1] >> (size % 8)) != 0) protocol_fault("bad padding");
  value.resize(size);
  for (Size i = 0; i < size; ++i) value[i] = (bits[i / 8] >> (i % 8)) & 1;
}
void decode(Reader& in, std::vector<int>& value) { decode_scalars(in, value); }
void decode(Reader& in, std::vector<long long>& value) { decode_scalars(in, value); }
void decode(Reader& in, std::vector<double>& value) { decode_scalars(in, value); }

Bytes& start_payload() {
  out_frame.resize(HEADER_BYTES);
  return out_frame;
}

const void* cached_result(uint16_t fn, Reader& in, Decoder decoder, Factory factory) {
  Tracked& entry = tracked_for(fn);
  entry.decoder = decoder;
  uint32_t id = in.u32();
  if (id < entry.objects.size()) {
    in.end();
    return entry.objects[id];
  }
  if (id != entry.objects.size()) protocol_fault("object id out of order");
  void* object = factory();
  decoder(in, object);
  in.end();
  entry.objects.push_back(object);
  return object;
}

Call::Call(uint16_t fn) : fn_(fn), serve_(contestant_serve), arguments_(start_payload()) {
  if (serve_ == 0) abort();  // a callback called before the contestant's main (in a static constructor)
}

Call::Call(uint16_t fn, Dispatch serve) : fn_(fn), serve_(serve), arguments_(start_payload()) {
  ignore_sigpipe();
}

void Call::done() { Reader(wait()).end(); }

const Bytes& Call::wait() {
  send(KIND_CALL, fn_);
  for (;;) {
    uint16_t kind;
    uint16_t fn;
    receive(kind, fn);
    if (kind == KIND_RETURN) {
      if (fn != fn_) protocol_fault("return for another call");
      return in_payload;
    }
    handle(kind, fn, serve_);
  }
}

void send_return(uint16_t fn) { send(KIND_RETURN, fn); }

void reply(uint16_t fn) {
  start_payload();
  send_return(fn);
}

void send_cached(uint16_t fn, const void* object, Encoder encoder) {
  if (remembered.size() <= fn) {
    remembered.resize(fn + 1u);
    remembered_ids.resize(fn + 1u);
  }
  Writer out(start_payload());
  std::map<const void*, uint32_t>::iterator known = remembered_ids[fn].find(object);
  if (known != remembered_ids[fn].end()) {
    out.u32(known->second);
    return send_return(fn);
  }
  uint32_t id = static_cast<uint32_t>(remembered[fn].size());
  remembered_ids[fn][object] = id;
  Remembered entry;
  entry.object = object;
  entry.encoder = encoder;
  remembered[fn].push_back(entry);
  {
    Serving charged(fn);  // the first copy of a new object is work for the contestant's call
    Writer sent(remembered[fn].back().sent);
    encoder(sent, object);
  }
  out.u32(id);
  out.raw(&remembered[fn].back().sent[0], remembered[fn].back().sent.size());
  remembers_any = true;
  send_return(fn);
}

Serving::Serving(uint16_t) {
  if (serving_depth++ != 0) return;
  if (!charge_opened) open_charge_page();
  if (charge_page == 0) return;
  uint64_t now = cpu_now_ns();
  charge_page->open_since_ns = now != 0 ? now : 1;
}

Serving::~Serving() {
  if (--serving_depth != 0 || charge_page == 0) return;
  uint64_t since = charge_page->open_since_ns;
  uint64_t now = cpu_now_ns();
  if (since != 0 && now > since) charge_page->charged_ns += now - since;
  charge_page->open_since_ns = 0;
}

void callback_threw(uint16_t) { abort(); }

void unknown_function(Request&) { protocol_fault("unknown function"); }

int contestant_main(Dispatch serve) {
  side = CONTESTANT;
  contestant_serve = serve;
  signal(SIGPIPE, SIG_IGN);
  atexit(send_exiting);
  for (;;) {
    uint16_t kind;
    uint16_t fn;
    receive(kind, fn);
    handle(kind, fn, serve);
  }
}

}  // namespace qhhoj
