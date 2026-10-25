// Split signature runtime: the calls between the jury box and the contestant box. Both generated sides
// (qhhoj_jury.cpp, qhhoj_contestant.cpp) call only the names below. C++03, and only <vector>, <string>, <utility>
// and <stdint.h>, so a problem header that declares a function named like a POSIX one never clashes; every
// system call is in qhhoj_rpc.cpp.
//
// Wire: each side reads fd 3 and writes fd 4 (two pipes the judge crossed). A frame is `len u32 | kind u16 |
// fn u16` and `len` payload bytes. Calls are synchronous: the caller sends CALL(fn) and serves every CALL that
// arrives until RETURN(fn). Values are encoded as below (native byte order: both boxes run on one host). A
// `const&` result is an object id (numbered per function in the order the jury first returned each object)
// followed, the first time only, by the value; CACHE(fn) carries an id and the new value of an object that
// changed.
#ifndef QHHOJ_RPC_HPP
#define QHHOJ_RPC_HPP

#include <stdint.h>

#include <string>
#include <utility>
#include <vector>

namespace qhhoj {

typedef std::vector<unsigned char> Bytes;
typedef Bytes::size_type Size;

enum Kind { KIND_CALL = 1, KIND_RETURN = 2, KIND_CACHE = 3, KIND_EXITING = 6 };

// A frame that breaks the protocol ends the side that read it: the jury records a protocol violation for the
// judge and exits; the contestant exits 117, which the judge grades as the contestant's own exit code.
void protocol_fault(const char* what) __attribute__((noreturn));

// Appends to a buffer it does not own (the runtime reuses its buffers, so a call allocates nothing). The
// two-buffer form first compares with `expected` and writes nothing while the encoding matches it; from the
// first difference `bytes` holds the whole encoding.
class Writer {
 public:
  explicit Writer(Bytes& bytes) : bytes_(&bytes), expected_(0), matched_(0) {}
  Writer(Bytes& bytes, const Bytes& expected) : bytes_(&bytes), expected_(&expected), matched_(0) {}
  void raw(const void* data, Size size) {
    const unsigned char* bytes = static_cast<const unsigned char*>(data);
    if (expected_ != 0 && still_matches(bytes, size)) return;
    bytes_->insert(bytes_->end(), bytes, bytes + size);
  }
  void u32(uint32_t value) { raw(&value, sizeof value); }
  // The encoding equals `expected` (the two-buffer form only).
  bool unchanged() const { return expected_ != 0 && matched_ == expected_->size(); }

 private:
  bool still_matches(const unsigned char* bytes, Size size);
  Bytes* bytes_;
  const Bytes* expected_;
  Size matched_;
};

// Reads a received payload; every read is checked against what is left, and the payload must be used up.
class Reader {
 public:
  explicit Reader(const Bytes& bytes) : bytes_(bytes), at_(0) {}
  void raw(void* out, Size size);
  uint32_t u32() {
    uint32_t value;
    raw(&value, sizeof value);
    return value;
  }
  // A count of items that each take at least `min_bytes`: refused before anything is allocated for them.
  uint32_t count(Size min_bytes) {
    uint32_t items = u32();
    if (min_bytes != 0 && items > left() / min_bytes) protocol_fault("count past the payload");
    return items;
  }
  Size left() const { return bytes_.size() - at_; }
  void end() const {
    if (at_ != bytes_.size()) protocol_fault("payload left over");
  }

 private:
  const Bytes& bytes_;
  Size at_;
};

// The fewest bytes one encoded T takes.
template <class T> struct MinSize;
template <> struct MinSize<int> { static const Size value = 4; };
template <> struct MinSize<long long> { static const Size value = 8; };
template <> struct MinSize<double> { static const Size value = 8; };
template <> struct MinSize<bool> { static const Size value = 1; };
template <> struct MinSize<std::string> { static const Size value = 4; };
template <class T> struct MinSize<std::vector<T> > { static const Size value = 4; };
template <class A, class B> struct MinSize<std::pair<A, B> > {
  static const Size value = MinSize<A>::value + MinSize<B>::value;
};

// Every overload is declared before any template uses it (names in templates bind at their definition). A
// vector of int, long long or double travels as one block; its bytes are the same as element by element.
void encode(Writer& out, int value);
void encode(Writer& out, long long value);
void encode(Writer& out, double value);
void encode(Writer& out, bool value);
void encode(Writer& out, const std::string& value);
void encode(Writer& out, const std::vector<bool>& value);
void encode(Writer& out, const std::vector<int>& value);
void encode(Writer& out, const std::vector<long long>& value);
void encode(Writer& out, const std::vector<double>& value);
template <class T> void encode(Writer& out, const std::vector<T>& value);
template <class A, class B> void encode(Writer& out, const std::pair<A, B>& value);
void decode(Reader& in, int& value);
void decode(Reader& in, long long& value);
void decode(Reader& in, double& value);
void decode(Reader& in, bool& value);
void decode(Reader& in, std::string& value);
void decode(Reader& in, std::vector<bool>& value);
void decode(Reader& in, std::vector<int>& value);
void decode(Reader& in, std::vector<long long>& value);
void decode(Reader& in, std::vector<double>& value);
template <class T> void decode(Reader& in, std::vector<T>& value);
template <class A, class B> void decode(Reader& in, std::pair<A, B>& value);

template <class T> void encode(Writer& out, const std::vector<T>& value) {
  out.u32(static_cast<uint32_t>(value.size()));
  for (Size i = 0; i < value.size(); ++i) encode(out, value[i]);
}

template <class A, class B> void encode(Writer& out, const std::pair<A, B>& value) {
  encode(out, value.first);
  encode(out, value.second);
}

// Decodes in place (a refreshed object keeps its address).
template <class T> void decode(Reader& in, std::vector<T>& value) {
  value.resize(in.count(MinSize<T>::value));
  for (Size i = 0; i < value.size(); ++i) decode(in, value[i]);
}

template <class A, class B> void decode(Reader& in, std::pair<A, B>& value) {
  decode(in, value.first);
  decode(in, value.second);
}

class Request;
typedef void (*Dispatch)(Request&);
typedef void (*Encoder)(Writer&, const void*);
typedef void (*Decoder)(Reader&, void*);
typedef void* (*Factory)();

template <class T> void encode_object(Writer& out, const void* object) {
  encode(out, *static_cast<const T*>(object));
}
template <class T> void decode_object(Reader& in, void* object) {
  decode(in, *static_cast<T*>(object));
}
template <class T> void* new_object() { return new T(); }

// The buffer the next frame's payload is written to, emptied.
Bytes& start_payload();
// The contestant's copy of the object a `const&` result names: one object per id, kept until exit.
const void* cached_result(uint16_t fn, Reader& in, Decoder decoder, Factory factory);

// One call to the other side. The contestant's proxies use the one-argument form (calls the jury makes while
// one is pending are served by the dispatcher `contestant_main` was given); the jury passes its dispatcher.
class Call {
 public:
  explicit Call(uint16_t fn);
  Call(uint16_t fn, Dispatch serve);
  template <class T> void put(const T& value) { encode(arguments_, value); }
  void done();
  template <class T> T result() {
    T value;
    Reader in(wait());
    decode(in, value);
    in.end();
    return value;
  }
  template <class T> const T& result_cached() {
    Reader in(wait());
    return *static_cast<const T*>(cached_result(fn_, in, &decode_object<T>, &new_object<T>));
  }

 private:
  const Bytes& wait();
  uint16_t fn_;
  Dispatch serve_;
  Writer arguments_;
};

// A call the other side made, being served. Its arguments are decoded before the call runs.
class Request {
 public:
  Request(uint16_t fn, const Bytes& payload) : fn_(fn), in_(payload) {}
  uint16_t fn() const { return fn_; }
  template <class T> T get() {
    T value;
    decode(in_, value);
    return value;
  }
  void end() { in_.end(); }

 private:
  uint16_t fn_;
  Reader in_;
};

void send_return(uint16_t fn);
void send_cached(uint16_t fn, const void* object, Encoder encoder);

void reply(uint16_t fn);
template <class T> void reply(uint16_t fn, const T& value) {
  Writer out(start_payload());
  encode(out, value);
  send_return(fn);
}
// A `const&` result: the jury remembers each object it returns and sends it again whenever it has changed
// (CACHE), so every reference the contestant holds sees its own object's live value, as in one process.
template <class T> void reply_cached(uint16_t fn, const T* value) { send_cached(fn, value, &encode_object<T>); }

// The jury's guard around one callback: the CPU the jury spends running the contestant's calls is recorded
// for the judge, which charges it to the contestant, as one process would have.
class Serving {
 public:
  explicit Serving(uint16_t fn);
  ~Serving();

 private:
  Serving(const Serving&);
  Serving& operator=(const Serving&);
};

// A callback that throws aborts the jury (an uncaught exception in one process ends it the same way).
void callback_threw(uint16_t fn) __attribute__((noreturn));
void unknown_function(Request& request) __attribute__((noreturn));
// The contestant's main: serves the jury's calls until the jury exits.
int contestant_main(Dispatch serve) __attribute__((noreturn));

}  // namespace qhhoj

#endif
