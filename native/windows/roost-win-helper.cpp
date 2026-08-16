#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#include <aclapi.h>
#include <bcrypt.h>
#include <iphlpapi.h>
#include <ntsecapi.h>
#include <sddl.h>
#include <shellapi.h>
#include <softpub.h>
#include <tlhelp32.h>
#include <userenv.h>
#include <wincrypt.h>
#include <winternl.h>
#include <wintrust.h>

#include <algorithm>
#include <array>
#include <atomic>
#include <chrono>
#include <cstdint>
#include <cstring>
#include <cwctype>
#include <limits>
#include <locale>
#include <map>
#include <optional>
#include <set>
#include <sstream>
#include <stdexcept>
#include <string>
#include <string_view>
#include <thread>
#include <unordered_map>
#include <utility>
#include <vector>

namespace roost {

constexpr std::uint32_t kProtocolVersion = 1;
constexpr std::uint32_t kMaxFrame = 16U * 1024U * 1024U;
constexpr std::uint64_t kMaxZipEntries = 100000;
constexpr std::uint64_t kMaxZipBytes = 64ULL * 1024ULL * 1024ULL * 1024ULL;

class Error final : public std::runtime_error {
 public:
  explicit Error(std::string value) : std::runtime_error(std::move(value)) {}
};


std::string toUtf8(const std::wstring& value) {
  if (value.empty()) return {};
  int n = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
      static_cast<int>(value.size()), nullptr, 0, nullptr, nullptr);
  if (!n) throw Error("invalid UTF-16");
  std::string out(static_cast<std::size_t>(n), '\0');
  if (!WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
      static_cast<int>(value.size()), out.data(), n, nullptr, nullptr)) throw Error("invalid UTF-16");
  return out;
}

std::wstring fromUtf8(std::string_view value) {
  if (value.empty()) return {};
  if (value.size() > static_cast<std::size_t>(INT_MAX)) throw Error("UTF-8 value is too large");
  int n = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
      static_cast<int>(value.size()), nullptr, 0);
  if (!n) throw Error("invalid UTF-8");
  std::wstring out(static_cast<std::size_t>(n), L'\0');
  if (!MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
      static_cast<int>(value.size()), out.data(), n)) throw Error("invalid UTF-8");
  return out;
}

std::string winText(DWORD code) {
  wchar_t* raw = nullptr;
  DWORD n = FormatMessageW(FORMAT_MESSAGE_ALLOCATE_BUFFER | FORMAT_MESSAGE_FROM_SYSTEM |
      FORMAT_MESSAGE_IGNORE_INSERTS, nullptr, code, 0, reinterpret_cast<wchar_t*>(&raw), 0, nullptr);
  std::wstring text = n && raw ? std::wstring(raw, n) : L"Windows error";
  if (raw) LocalFree(raw);
  while (!text.empty() && (text.back() == L'\r' || text.back() == L'\n' || text.back() == L' ')) text.pop_back();
  return toUtf8(text) + " [win32=" + std::to_string(code) + "]";
}

[[noreturn]] void failWin(const char* where, DWORD code = GetLastError()) {
  throw Error(std::string(where) + ": " + winText(code));
}
[[noreturn]] void failLsa(const char* where, NTSTATUS status) { failWin(where, LsaNtStatusToWinError(status)); }

class Handle final {
 public:
  explicit Handle(HANDLE value = nullptr) : value_(value) {}
  ~Handle() { reset(); }
  Handle(const Handle&) = delete;
  Handle& operator=(const Handle&) = delete;
  Handle(Handle&& other) noexcept : value_(other.release()) {}
  Handle& operator=(Handle&& other) noexcept { if (this != &other) reset(other.release()); return *this; }
  HANDLE get() const { return value_; }
  explicit operator bool() const { return value_ && value_ != INVALID_HANDLE_VALUE; }
  HANDLE release() { HANDLE out = value_; value_ = nullptr; return out; }
  void reset(HANDLE value = nullptr) { if (value_ && value_ != INVALID_HANDLE_VALUE) CloseHandle(value_); value_ = value; }
 private:
  HANDLE value_;
};

class ServiceHandle final {
 public:
  explicit ServiceHandle(SC_HANDLE value = nullptr) : value_(value) {}
  ~ServiceHandle() { if (value_) CloseServiceHandle(value_); }
  ServiceHandle(const ServiceHandle&) = delete;
  ServiceHandle& operator=(const ServiceHandle&) = delete;
  SC_HANDLE get() const { return value_; }
  explicit operator bool() const { return value_ != nullptr; }
 private:
  SC_HANDLE value_;
};

template<class T> class Local final {
 public:
  explicit Local(T* value = nullptr) : value_(value) {}
  ~Local() { if (value_) LocalFree(value_); }
  Local(const Local&) = delete;
  Local& operator=(const Local&) = delete;
  T* get() const { return value_; }
 private:
  T* value_;
};

class SecureBytes final {
 public:
  explicit SecureBytes(std::vector<std::uint8_t> value) : value_(std::move(value)) {}
  ~SecureBytes() { if (!value_.empty()) SecureZeroMemory(value_.data(), value_.size()); }
  std::vector<std::uint8_t>& get() { return value_; }
 private:
  std::vector<std::uint8_t> value_;
};

class SecureWide final {
 public:
  explicit SecureWide(std::wstring value) : value_(std::move(value)) {}
  ~SecureWide() { if (!value_.empty()) SecureZeroMemory(value_.data(), value_.size() * sizeof(wchar_t)); }
  const wchar_t* c_str() const { return value_.c_str(); }
 private:
  std::wstring value_;
};

std::string json(std::string_view value) {
  static constexpr char hex[] = "0123456789abcdef";
  std::string out(1, '"');
  for (unsigned char ch : value) {
    switch (ch) {
      case '"': out += "\\\""; break;
      case '\\': out += "\\\\"; break;
      case '\b': out += "\\b"; break;
      case '\f': out += "\\f"; break;
      case '\n': out += "\\n"; break;
      case '\r': out += "\\r"; break;
      case '\t': out += "\\t"; break;
      default:
        if (ch < 0x20) { out += "\\u00"; out.push_back(hex[ch >> 4]); out.push_back(hex[ch & 15]); }
        else out.push_back(static_cast<char>(ch));
    }
  }
  out.push_back('"');
  return out;
}
std::string json(const std::wstring& value) { return json(toUtf8(value)); }

void writeAll(HANDLE target, const void* value, std::size_t size) {
  auto* cursor = static_cast<const std::uint8_t*>(value);
  while (size) {
    DWORD done = 0;
    DWORD chunk = static_cast<DWORD>(std::min<std::size_t>(size, 1U << 20));
    if (!WriteFile(target, cursor, chunk, &done, nullptr)) failWin("WriteFile");
    if (!done) throw Error("WriteFile made no progress");
    cursor += done;
    size -= done;
  }
}
void emit(std::string value) { value.push_back('\n'); writeAll(GetStdHandle(STD_OUTPUT_HANDLE), value.data(), value.size()); }
void expect(const std::vector<std::wstring>& args, std::size_t n, const char* usage) {
  if (args.size() != n) throw Error(std::string("usage: ") + usage);
}

std::uint32_t uint32Arg(const std::wstring& text, const char* label) {
  if (text.empty()) throw Error(std::string("invalid ") + label);
  std::uint64_t value = 0;
  for (wchar_t ch : text) {
    if (ch < L'0' || ch > L'9') throw Error(std::string("invalid ") + label);
    value = value * 10 + static_cast<unsigned>(ch - L'0');
    if (value > UINT32_MAX) throw Error(std::string("invalid ") + label);
  }
  if (!value) throw Error(std::string("invalid ") + label);
  return static_cast<std::uint32_t>(value);
}

std::vector<std::uint8_t> framedInput(std::uint32_t maximum = kMaxFrame) {
  HANDLE in = GetStdHandle(STD_INPUT_HANDLE);
  if (!in || in == INVALID_HANDLE_VALUE) throw Error("framed stdin unavailable");
  std::array<std::uint8_t, 4> prefix{};
  std::size_t offset = 0;
  while (offset < prefix.size()) {
    DWORD got = 0;
    if (!ReadFile(in, prefix.data() + offset, static_cast<DWORD>(prefix.size() - offset), &got, nullptr)) failWin("ReadFile(stdin)");
    if (!got) throw Error("truncated framed stdin");
    offset += got;
  }
  std::uint32_t length = prefix[0] | (std::uint32_t(prefix[1]) << 8) |
      (std::uint32_t(prefix[2]) << 16) | (std::uint32_t(prefix[3]) << 24);
  if (length > maximum) throw Error("framed stdin exceeds command limit");
  std::vector<std::uint8_t> out(length);
  offset = 0;
  while (offset < out.size()) {
    DWORD got = 0;
    DWORD want = static_cast<DWORD>(std::min<std::size_t>(out.size() - offset, 1U << 20));
    if (!ReadFile(in, out.data() + offset, want, &got, nullptr)) failWin("ReadFile(stdin)");
    if (!got) throw Error("truncated framed stdin payload");
    offset += got;
  }
  std::uint8_t extra = 0;
  DWORD got = 0;
  if (!ReadFile(in, &extra, 1, &got, nullptr) && GetLastError() != ERROR_BROKEN_PIPE) failWin("ReadFile(stdin)");
  if (got) throw Error("multiple framed stdin payloads are not allowed");
  return out;
}

std::wstring lower(const std::wstring& value) {
  if (value.empty()) return {};
  int n = LCMapStringEx(LOCALE_NAME_INVARIANT, LCMAP_LOWERCASE, value.data(),
      static_cast<int>(value.size()), nullptr, 0, nullptr, nullptr, 0);
  if (!n) failWin("LCMapStringEx");
  std::wstring out(static_cast<std::size_t>(n), L'\0');
  if (!LCMapStringEx(LOCALE_NAME_INVARIANT, LCMAP_LOWERCASE, value.data(),
      static_cast<int>(value.size()), out.data(), n, nullptr, nullptr, 0)) failWin("LCMapStringEx");
  return out;
}

std::vector<std::uint8_t> currentSid() {
  HANDLE raw = nullptr;
  if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw)) failWin("OpenProcessToken");
  Handle token(raw);
  DWORD bytes = 0;
  GetTokenInformation(token.get(), TokenUser, nullptr, 0, &bytes);
  if (GetLastError() != ERROR_INSUFFICIENT_BUFFER) failWin("GetTokenInformation");
  std::vector<std::uint8_t> buffer(bytes);
  if (!GetTokenInformation(token.get(), TokenUser, buffer.data(), bytes, &bytes)) failWin("GetTokenInformation");
  auto* sid = reinterpret_cast<TOKEN_USER*>(buffer.data())->User.Sid;
  std::vector<std::uint8_t> out(GetLengthSid(sid));
  if (!CopySid(static_cast<DWORD>(out.size()), out.data(), sid)) failWin("CopySid");
  return out;
}

std::wstring sidText(PSID sid) {
  wchar_t* raw = nullptr;
  if (!ConvertSidToStringSidW(sid, &raw)) failWin("ConvertSidToStringSidW");
  Local<wchar_t> value(raw);
  return value.get();
}

void applyPrivateDacl(const std::wstring& path) {
  auto sid = currentSid();
  std::wstring sddl = L"D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;" + sidText(sid.data()) + L")";
  PSECURITY_DESCRIPTOR raw = nullptr;
  if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.c_str(), SDDL_REVISION_1, &raw, nullptr)) {
    failWin("ConvertStringSecurityDescriptorToSecurityDescriptorW");
  }
  Local<SECURITY_DESCRIPTOR> descriptor(static_cast<SECURITY_DESCRIPTOR*>(raw));
  BOOL present = FALSE;
  BOOL defaulted = FALSE;
  PACL dacl = nullptr;
  if (!GetSecurityDescriptorDacl(descriptor.get(), &present, &dacl, &defaulted) || !present) {
    failWin("GetSecurityDescriptorDacl");
  }
  DWORD code = SetNamedSecurityInfoW(const_cast<wchar_t*>(path.c_str()), SE_FILE_OBJECT,
      DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
      nullptr, nullptr, dacl, nullptr);
  if (code != ERROR_SUCCESS) failWin("SetNamedSecurityInfoW", code);
}

void flushFile(const std::vector<std::wstring>& args) {
  expect(args, 1, "flush-file <path>");
  Handle file(CreateFileW(args[0].c_str(), GENERIC_WRITE,
      FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr, OPEN_EXISTING,
      FILE_ATTRIBUTE_NORMAL, nullptr));
  if (!file) failWin("CreateFileW");
  if (!FlushFileBuffers(file.get())) failWin("FlushFileBuffers");
  emit("{\"ok\":true}");
}

void replaceFile(const std::vector<std::wstring>& args) {
  expect(args, 2, "replace-file <source> <destination>");
  if (!ReplaceFileW(args[1].c_str(), args[0].c_str(), nullptr, REPLACEFILE_WRITE_THROUGH, nullptr, nullptr)) {
    DWORD code = GetLastError();
    if (code != ERROR_FILE_NOT_FOUND && code != ERROR_PATH_NOT_FOUND) failWin("ReplaceFileW", code);
    if (!MoveFileExW(args[0].c_str(), args[1].c_str(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)) {
      failWin("MoveFileExW");
    }
  }
  emit("{\"ok\":true}");
}

void removeFile(const std::vector<std::wstring>& args) {
  expect(args, 1, "remove-file <path>");
  if (!DeleteFileW(args[0].c_str())) {
    DWORD code = GetLastError();
    if (code != ERROR_FILE_NOT_FOUND && code != ERROR_PATH_NOT_FOUND) failWin("DeleteFileW", code);
  }
  emit("{\"ok\":true}");
}

void applyDacl(const std::vector<std::wstring>& args) {
  expect(args, 1, "apply-dacl <path>");
  applyPrivateDacl(args[0]);
  emit("{\"ok\":true}");
}

void getDacl(const std::vector<std::wstring>& args) {
  expect(args, 1, "get-dacl <path>");
  PSECURITY_DESCRIPTOR raw = nullptr;
  PACL dacl = nullptr;
  DWORD code = GetNamedSecurityInfoW(const_cast<wchar_t*>(args[0].c_str()), SE_FILE_OBJECT,
      DACL_SECURITY_INFORMATION, nullptr, nullptr, &dacl, nullptr, &raw);
  if (code != ERROR_SUCCESS) failWin("GetNamedSecurityInfoW", code);
  Local<SECURITY_DESCRIPTOR> descriptor(static_cast<SECURITY_DESCRIPTOR*>(raw));
  wchar_t* text = nullptr;
  if (!ConvertSecurityDescriptorToStringSecurityDescriptorW(descriptor.get(), SDDL_REVISION_1,
      DACL_SECURITY_INFORMATION, &text, nullptr)) failWin("ConvertSecurityDescriptorToStringSecurityDescriptorW");
  Local<wchar_t> sddl(text);
  emit("{\"sddl\":" + json(std::wstring(sddl.get())) + "}");
}

void applySddl(const std::vector<std::wstring>& args) {
  expect(args, 2, "apply-sddl <path> <sddl>");
  PSECURITY_DESCRIPTOR raw = nullptr;
  if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(args[1].c_str(), SDDL_REVISION_1, &raw, nullptr)) {
    failWin("ConvertStringSecurityDescriptorToSecurityDescriptorW");
  }
  Local<SECURITY_DESCRIPTOR> descriptor(static_cast<SECURITY_DESCRIPTOR*>(raw));
  BOOL present = FALSE;
  BOOL defaulted = FALSE;
  PACL dacl = nullptr;
  if (!GetSecurityDescriptorDacl(descriptor.get(), &present, &dacl, &defaulted) || !present) {
    throw Error("SDDL must contain a DACL");
  }
  SECURITY_DESCRIPTOR_CONTROL control = 0;
  DWORD revision = 0;
  if (!GetSecurityDescriptorControl(descriptor.get(), &control, &revision)) failWin("GetSecurityDescriptorControl");
  SECURITY_INFORMATION info = DACL_SECURITY_INFORMATION |
      ((control & SE_DACL_PROTECTED) ? PROTECTED_DACL_SECURITY_INFORMATION : UNPROTECTED_DACL_SECURITY_INFORMATION);
  DWORD code = SetNamedSecurityInfoW(const_cast<wchar_t*>(args[0].c_str()), SE_FILE_OBJECT,
      info, nullptr, nullptr, dacl, nullptr);
  if (code != ERROR_SUCCESS) failWin("SetNamedSecurityInfoW", code);
  emit("{\"ok\":true}");
}

void exclusiveOpen(const std::vector<std::wstring>& args) {
  expect(args, 1, "probe-exclusive-open <path>");
  Handle file(CreateFileW(args[0].c_str(), GENERIC_READ, 0, nullptr, OPEN_EXISTING,
      FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT, nullptr));
  if (file) return emit("{\"exclusive\":true}");
  DWORD code = GetLastError();
  if (code == ERROR_SHARING_VIOLATION || code == ERROR_LOCK_VIOLATION || code == ERROR_ACCESS_DENIED) {
    return emit("{\"exclusive\":false}");
  }
  failWin("CreateFileW", code);
}

void currentUserSid(const std::vector<std::wstring>& args) {
  expect(args, 0, "current-user-sid");
  auto sid = currentSid();
  emit("{\"sid\":" + json(sidText(sid.data())) + "}");
}

std::uint64_t fileTime(const FILETIME& value) {
  return (std::uint64_t(value.dwHighDateTime) << 32) | value.dwLowDateTime;
}

void hostSample(const std::vector<std::wstring>& args) {
  expect(args, 0, "host-sample");
  FILETIME idle1{}, kernel1{}, user1{}, idle2{}, kernel2{}, user2{};
  if (!GetSystemTimes(&idle1, &kernel1, &user1)) failWin("GetSystemTimes");
  Sleep(100);
  if (!GetSystemTimes(&idle2, &kernel2, &user2)) failWin("GetSystemTimes");
  std::uint64_t idle = fileTime(idle2) - fileTime(idle1);
  std::uint64_t kernel = fileTime(kernel2) - fileTime(kernel1);
  std::uint64_t user = fileTime(user2) - fileTime(user1);
  std::uint64_t totalTicks = kernel + user;
  double cpu = totalTicks ? 100.0 * double(totalTicks - std::min(idle, totalTicks)) / double(totalTicks) : 0.0;

  MEMORYSTATUSEX memory{};
  memory.dwLength = sizeof(memory);
  if (!GlobalMemoryStatusEx(&memory)) failWin("GlobalMemoryStatusEx");
  wchar_t windowsDir[MAX_PATH]{};
  if (!GetWindowsDirectoryW(windowsDir, MAX_PATH)) failWin("GetWindowsDirectoryW");
  std::wstring root(windowsDir);
  if (root.size() < 3 || root[1] != L':') throw Error("Windows directory is not on a local volume");
  root.resize(3);
  ULARGE_INTEGER available{}, diskTotal{}, diskFree{};
  if (!GetDiskFreeSpaceExW(root.c_str(), &available, &diskTotal, &diskFree)) failWin("GetDiskFreeSpaceExW");

  PMIB_IF_TABLE2 table = nullptr;
  DWORD code = GetIfTable2(&table);
  if (code != NO_ERROR) failWin("GetIfTable2", code);
  std::uint64_t rx = 0, tx = 0;
  for (ULONG i = 0; i < table->NumEntries; ++i) {
    const auto& row = table->Table[i];
    if (row.Type == IF_TYPE_SOFTWARE_LOOPBACK || row.OperStatus != IfOperStatusUp) continue;
    rx += row.InOctets;
    tx += row.OutOctets;
  }
  FreeMibTable(table);
  std::ostringstream out;
  out.imbue(std::locale::classic());
  out.precision(6);
  out << "{\"cpu_pct\":" << cpu
      << ",\"mem_used_bytes\":" << (memory.ullTotalPhys - memory.ullAvailPhys)
      << ",\"mem_total_bytes\":" << memory.ullTotalPhys
      << ",\"disk_used_bytes\":" << (diskTotal.QuadPart - diskFree.QuadPart)
      << ",\"disk_total_bytes\":" << diskTotal.QuadPart
      << ",\"net\":{\"rxBytes\":" << rx << ",\"txBytes\":" << tx << "}}";
  emit(out.str());
}

std::wstring processImage(DWORD pid) {
  Handle process(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid));
  if (!process) return {};
  std::wstring value(32768, L'\0');
  DWORD length = static_cast<DWORD>(value.size());
  if (!QueryFullProcessImageNameW(process.get(), 0, value.data(), &length)) return {};
  value.resize(length);
  return value;
}

std::wstring baseName(const std::wstring& path) {
  std::size_t slash = path.find_last_of(L"\\/");
  return slash == std::wstring::npos ? path : path.substr(slash + 1);
}

std::wstring commandLineFor(DWORD pid) {
  Handle process(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid));
  if (!process) return {};
  using Query = NTSTATUS(NTAPI*)(HANDLE, PROCESSINFOCLASS, PVOID, ULONG, PULONG);
  auto query = reinterpret_cast<Query>(GetProcAddress(GetModuleHandleW(L"ntdll.dll"), "NtQueryInformationProcess"));
  if (!query) return {};
  ULONG needed = 0;
  query(process.get(), static_cast<PROCESSINFOCLASS>(60), nullptr, 0, &needed);
  if (needed < sizeof(UNICODE_STRING) || needed > kMaxFrame) return {};
  std::vector<std::uint8_t> buffer(needed);
  NTSTATUS status = query(process.get(), static_cast<PROCESSINFOCLASS>(60), buffer.data(), needed, &needed);
  if (status < 0) return {};
  auto* line = reinterpret_cast<UNICODE_STRING*>(buffer.data());
  if (!line->Buffer || line->Length % sizeof(wchar_t)) return {};
  return std::wstring(line->Buffer, line->Length / sizeof(wchar_t));
}

void processSnapshot(const std::vector<std::wstring>& args) {
  expect(args, 0, "process-snapshot");
  Handle snapshot(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0));
  if (!snapshot) failWin("CreateToolhelp32Snapshot");
  PROCESSENTRY32W entry{};
  entry.dwSize = sizeof(entry);
  std::string out = "[";
  bool first = true;
  if (Process32FirstW(snapshot.get(), &entry)) {
    do {
      if (!entry.th32ProcessID) continue;
      std::wstring image = processImage(entry.th32ProcessID);
      std::wstring line = commandLineFor(entry.th32ProcessID);
      std::wstring comm = image.empty() ? std::wstring(entry.szExeFile) : baseName(image);
      if (!first) out.push_back(',');
      first = false;
      out += "{\"pid\":" + std::to_string(entry.th32ProcessID) +
          ",\"ppid\":" + std::to_string(entry.th32ParentProcessID) +
          ",\"pgid\":" + std::to_string(entry.th32ProcessID) +
          ",\"tpgid\":" + std::to_string(entry.th32ProcessID) +
          ",\"comm\":" + json(comm) +
          ",\"args\":" + json(line.empty() ? image : line) + "}";
    } while (Process32NextW(snapshot.get(), &entry));
    if (GetLastError() != ERROR_NO_MORE_FILES) failWin("Process32NextW");
  } else if (GetLastError() != ERROR_NO_MORE_FILES) {
    failWin("Process32FirstW");
  }
  out.push_back(']');
  emit(std::move(out));
}

class Winsock final {
 public:
  Winsock() {
    WSADATA data{};
    int code = WSAStartup(MAKEWORD(2, 2), &data);
    if (code) failWin("WSAStartup", static_cast<DWORD>(code));
  }
  ~Winsock() { WSACleanup(); }
};

void listeningPorts(const std::vector<std::wstring>& args) {
  expect(args, 0, "listening-ports");
  Winsock winsock;
  std::string out = "[";
  bool first = true;
  ULONG bytes = 0;
  DWORD code = GetExtendedTcpTable(nullptr, &bytes, FALSE, AF_INET, TCP_TABLE_OWNER_PID_ALL, 0);
  if (code != ERROR_INSUFFICIENT_BUFFER) failWin("GetExtendedTcpTable(IPv4)", code);
  std::vector<std::uint8_t> storage(bytes);
  code = GetExtendedTcpTable(storage.data(), &bytes, FALSE, AF_INET, TCP_TABLE_OWNER_PID_ALL, 0);
  if (code != NO_ERROR) failWin("GetExtendedTcpTable(IPv4)", code);
  auto* v4 = reinterpret_cast<MIB_TCPTABLE_OWNER_PID*>(storage.data());
  for (DWORD i = 0; i < v4->dwNumEntries; ++i) {
    const auto& row = v4->table[i];
    if (row.dwState != MIB_TCP_STATE_LISTEN || !row.dwOwningPid) continue;
    IN_ADDR address{};
    address.S_un.S_addr = row.dwLocalAddr;
    wchar_t text[INET_ADDRSTRLEN]{};
    if (!InetNtopW(AF_INET, &address, text, INET_ADDRSTRLEN)) failWin("InetNtopW");
    if (!first) out.push_back(',');
    first = false;
    out += "{\"pid\":" + std::to_string(row.dwOwningPid) + ",\"address\":" +
        json(std::wstring(text)) + ",\"port\":" +
        std::to_string(ntohs(static_cast<u_short>(row.dwLocalPort))) + "}";
  }
  bytes = 0;
  code = GetExtendedTcpTable(nullptr, &bytes, FALSE, AF_INET6, TCP_TABLE_OWNER_PID_ALL, 0);
  if (code != ERROR_INSUFFICIENT_BUFFER) failWin("GetExtendedTcpTable(IPv6)", code);
  storage.resize(bytes);
  code = GetExtendedTcpTable(storage.data(), &bytes, FALSE, AF_INET6, TCP_TABLE_OWNER_PID_ALL, 0);
  if (code != NO_ERROR) failWin("GetExtendedTcpTable(IPv6)", code);
  auto* v6 = reinterpret_cast<MIB_TCP6TABLE_OWNER_PID*>(storage.data());
  for (DWORD i = 0; i < v6->dwNumEntries; ++i) {
    const auto& row = v6->table[i];
    if (row.dwState != MIB_TCP_STATE_LISTEN || !row.dwOwningPid) continue;
    IN6_ADDR address{};
    std::memcpy(&address, row.ucLocalAddr, sizeof(address));
    wchar_t text[INET6_ADDRSTRLEN]{};
    if (!InetNtopW(AF_INET6, &address, text, INET6_ADDRSTRLEN)) failWin("InetNtopW");
    std::wstring rendered(text);
    if (row.dwLocalScopeId) rendered += L"%" + std::to_wstring(row.dwLocalScopeId);
    if (!first) out.push_back(',');
    first = false;
    out += "{\"pid\":" + std::to_string(row.dwOwningPid) + ",\"address\":" +
        json(rendered) + ",\"port\":" +
        std::to_string(ntohs(static_cast<u_short>(row.dwLocalPort))) + "}";
  }
  out.push_back(']');
  emit(std::move(out));
}

std::array<std::uint8_t, 32> certificateHash(PCCERT_CONTEXT certificate) {
  std::array<std::uint8_t, 32> out{};
  DWORD size = static_cast<DWORD>(out.size());
  if (!CryptHashCertificate2(BCRYPT_SHA256_ALGORITHM, 0, nullptr,
      certificate->pbCertEncoded, certificate->cbCertEncoded, out.data(), &size) || size != out.size()) {
    failWin("CryptHashCertificate2");
  }
  return out;
}

std::string hexHash(const std::array<std::uint8_t, 32>& value) {
  static constexpr char digits[] = "0123456789abcdef";
  std::string out;
  out.reserve(64);
  for (std::uint8_t byte : value) {
    out.push_back(digits[byte >> 4]);
    out.push_back(digits[byte & 15]);
  }
  return out;
}

std::string checkedPublisher(const std::wstring& text) {
  std::string value = toUtf8(text);
  if (value.size() != 64) throw Error("publisher SHA-256 must contain 64 hexadecimal characters");
  for (char& ch : value) {
    if (ch >= 'A' && ch <= 'F') ch = static_cast<char>(ch - 'A' + 'a');
    if (!((ch >= '0' && ch <= '9') || (ch >= 'a' && ch <= 'f'))) throw Error("invalid publisher SHA-256");
  }
  return value;
}

bool signerTimestamped(HCRYPTMSG message, DWORD index) {
  DWORD bytes = 0;
  if (!CryptMsgGetParam(message, CMSG_SIGNER_INFO_PARAM, index, nullptr, &bytes)) return false;
  std::vector<std::uint8_t> buffer(bytes);
  if (!CryptMsgGetParam(message, CMSG_SIGNER_INFO_PARAM, index, buffer.data(), &bytes)) return false;
  auto* signer = reinterpret_cast<CMSG_SIGNER_INFO*>(buffer.data());
  for (DWORD i = 0; i < signer->UnauthAttrs.cAttr; ++i) {
    const char* oid = signer->UnauthAttrs.rgAttr[i].pszObjId;
    if (oid && (!std::strcmp(oid, szOID_RSA_counterSign) ||
                !std::strcmp(oid, "1.3.6.1.4.1.311.3.3.1"))) return true;
  }
  return false;
}

void verifyCertificateChain(PCCERT_CONTEXT certificate) {
  LPSTR usage = const_cast<LPSTR>(szOID_KP_CODE_SIGNING);
  CERT_CHAIN_PARA parameters{};
  parameters.cbSize = sizeof(parameters);
  parameters.RequestedUsage.dwType = USAGE_MATCH_TYPE_AND;
  parameters.RequestedUsage.Usage.cUsageIdentifier = 1;
  parameters.RequestedUsage.Usage.rgpszUsageIdentifier = &usage;
  PCCERT_CHAIN_CONTEXT raw = nullptr;
  if (!CertGetCertificateChain(nullptr, certificate, nullptr, certificate->hCertStore,
      &parameters, CERT_CHAIN_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT, nullptr, &raw)) failWin("CertGetCertificateChain");
  struct ChainGuard { PCCERT_CHAIN_CONTEXT value; ~ChainGuard() { CertFreeCertificateChain(value); } } chain{raw};
  CERT_CHAIN_POLICY_PARA policy{};
  policy.cbSize = sizeof(policy);
  CERT_CHAIN_POLICY_STATUS status{};
  status.cbSize = sizeof(status);
  if (!CertVerifyCertificateChainPolicy(CERT_CHAIN_POLICY_AUTHENTICODE, raw, &policy, &status)) {
    failWin("CertVerifyCertificateChainPolicy");
  }
  if (status.dwError) failWin("untrusted signing certificate", status.dwError);
}

std::vector<std::uint8_t> readWholeFile(const std::wstring& path, std::uint64_t maximum) {
  Handle file(CreateFileW(path.c_str(), GENERIC_READ, FILE_SHARE_READ, nullptr, OPEN_EXISTING,
      FILE_FLAG_SEQUENTIAL_SCAN, nullptr));
  if (!file) failWin("CreateFileW");
  LARGE_INTEGER size{};
  if (!GetFileSizeEx(file.get(), &size)) failWin("GetFileSizeEx");
  if (size.QuadPart < 0 || static_cast<std::uint64_t>(size.QuadPart) > maximum ||
      static_cast<std::uint64_t>(size.QuadPart) > UINT32_MAX) throw Error("signed input exceeds verification limit");
  std::vector<std::uint8_t> out(static_cast<std::size_t>(size.QuadPart));
  std::size_t offset = 0;
  while (offset < out.size()) {
    DWORD got = 0;
    DWORD want = static_cast<DWORD>(std::min<std::size_t>(out.size() - offset, 1U << 20));
    if (!ReadFile(file.get(), out.data() + offset, want, &got, nullptr)) failWin("ReadFile");
    if (!got) throw Error("signed input was truncated");
    offset += got;
  }
  return out;
}

void verifyDetachedCms(const std::vector<std::wstring>& args) {
  if (args.size() != 4 || args[2] != L"--publisher-sha256") {
    throw Error("usage: verify-cms-detached <manifest> <signature> --publisher-sha256 <sha256>");
  }
  std::string expectedHash = checkedPublisher(args[3]);
  auto content = readWholeFile(args[0], 512ULL * 1024ULL * 1024ULL);
  auto signature = readWholeFile(args[1], 64ULL * 1024ULL * 1024ULL);
  CRYPT_VERIFY_MESSAGE_PARA parameters{};
  parameters.cbSize = sizeof(parameters);
  parameters.dwMsgAndCertEncodingType = X509_ASN_ENCODING | PKCS_7_ASN_ENCODING;
  const BYTE* parts[] = {content.data()};
  DWORD sizes[] = {static_cast<DWORD>(content.size())};
  PCCERT_CONTEXT matched = nullptr;
  DWORD matchedIndex = 0;
  for (DWORD index = 0;; ++index) {
    PCCERT_CONTEXT certificate = nullptr;
    if (!CryptVerifyDetachedMessageSignature(&parameters, index, signature.data(),
        static_cast<DWORD>(signature.size()), 1, parts, sizes, &certificate)) {
      DWORD code = GetLastError();
      if (code == CRYPT_E_INVALID_INDEX) break;
      continue;
    }
    if (hexHash(certificateHash(certificate)) == expectedHash) {
      matched = certificate;
      matchedIndex = index;
      break;
    }
    CertFreeCertificateContext(certificate);
  }
  if (!matched) throw Error("detached CMS signature did not match the pinned publisher");
  struct CertGuard { PCCERT_CONTEXT value; ~CertGuard() { CertFreeCertificateContext(value); } } cert{matched};
  verifyCertificateChain(matched);
  HCERTSTORE store = nullptr;
  HCRYPTMSG message = nullptr;
  DWORD encoding = 0, contentType = 0, format = 0;
  if (!CryptQueryObject(CERT_QUERY_OBJECT_FILE, args[1].c_str(),
      CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED, CERT_QUERY_FORMAT_FLAG_BINARY, 0,
      &encoding, &contentType, &format, &store, &message, nullptr)) failWin("CryptQueryObject");
  struct QueryGuard {
    HCERTSTORE store;
    HCRYPTMSG message;
    ~QueryGuard() { if (message) CryptMsgClose(message); if (store) CertCloseStore(store, 0); }
  } query{store, message};
  bool timestamped = signerTimestamped(message, matchedIndex);
  emit("{\"valid\":true,\"publisherSha256\":" + json(expectedHash) +
      ",\"timestamped\":" + std::string(timestamped ? "true" : "false") + "}");
}

void verifyAuthenticode(const std::vector<std::wstring>& args) {
  if (args.size() != 3 || args[1] != L"--publisher-sha256") {
    throw Error("usage: verify-authenticode <asset> --publisher-sha256 <sha256>");
  }
  std::string expectedHash = checkedPublisher(args[2]);
  WINTRUST_FILE_INFO file{};
  file.cbStruct = sizeof(file);
  file.pcwszFilePath = args[0].c_str();
  WINTRUST_DATA trust{};
  trust.cbStruct = sizeof(trust);
  trust.dwUIChoice = WTD_UI_NONE;
  trust.fdwRevocationChecks = WTD_REVOKE_WHOLECHAIN;
  trust.dwUnionChoice = WTD_CHOICE_FILE;
  trust.pFile = &file;
  trust.dwStateAction = WTD_STATEACTION_VERIFY;
  trust.dwProvFlags = WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT;
  GUID action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
  LONG result = WinVerifyTrust(nullptr, &action, &trust);
  bool valid = result == ERROR_SUCCESS;
  std::string actual;
  bool timestamped = false;
  if (valid) {
    CRYPT_PROVIDER_DATA* data = WTHelperProvDataFromStateData(trust.hWVTStateData);
    CRYPT_PROVIDER_SGNR* signer = data ? WTHelperGetProvSignerFromChain(data, 0, FALSE, 0) : nullptr;
    if (!signer || !signer->csCertChain || !signer->pasCertChain[0].pCert) {
      valid = false;
    } else {
      actual = hexHash(certificateHash(signer->pasCertChain[0].pCert));
      timestamped = signer->csCounterSigners > 0;
      valid = actual == expectedHash;
    }
  }
  trust.dwStateAction = WTD_STATEACTION_CLOSE;
  WinVerifyTrust(nullptr, &action, &trust);
  if (!valid) throw Error("Authenticode verification failed or publisher did not match");
  emit("{\"valid\":true,\"publisherSha256\":" + json(actual) +
      ",\"timestamped\":" + std::string(timestamped ? "true" : "false") + "}");
}

struct JsonValue {
  enum class Type { Null, Bool, Number, String, Array, Object } type = Type::Null;
  bool boolean = false;
  std::uint64_t number = 0;
  std::string string;
  std::vector<JsonValue> array;
  std::map<std::string, JsonValue> object;
};

void appendCodePoint(std::string& out, std::uint32_t code) {
  if (code <= 0x7f) out.push_back(static_cast<char>(code));
  else if (code <= 0x7ff) {
    out.push_back(static_cast<char>(0xc0 | (code >> 6)));
    out.push_back(static_cast<char>(0x80 | (code & 0x3f)));
  } else if (code <= 0xffff) {
    out.push_back(static_cast<char>(0xe0 | (code >> 12)));
    out.push_back(static_cast<char>(0x80 | ((code >> 6) & 0x3f)));
    out.push_back(static_cast<char>(0x80 | (code & 0x3f)));
  } else {
    out.push_back(static_cast<char>(0xf0 | (code >> 18)));
    out.push_back(static_cast<char>(0x80 | ((code >> 12) & 0x3f)));
    out.push_back(static_cast<char>(0x80 | ((code >> 6) & 0x3f)));
    out.push_back(static_cast<char>(0x80 | (code & 0x3f)));
  }
}

class JsonParser final {
 public:
  explicit JsonParser(std::string_view input) : input_(input) {}
  JsonValue parse() {
    JsonValue value = parseValue();
    whitespace();
    if (position_ != input_.size()) throw Error("trailing data in JSON input");
    return value;
  }
 private:
  void whitespace() {
    while (position_ < input_.size() && (input_[position_] == ' ' || input_[position_] == '\t' ||
        input_[position_] == '\r' || input_[position_] == '\n')) ++position_;
  }
  bool take(char ch) {
    whitespace();
    if (position_ < input_.size() && input_[position_] == ch) { ++position_; return true; }
    return false;
  }
  void literal(std::string_view value) {
    if (input_.substr(position_, value.size()) != value) throw Error("invalid JSON token");
    position_ += value.size();
  }
  std::uint32_t hex4() {
    if (position_ + 4 > input_.size()) throw Error("truncated JSON Unicode escape");
    std::uint32_t value = 0;
    for (int i = 0; i < 4; ++i) {
      char ch = input_[position_++];
      value <<= 4;
      if (ch >= '0' && ch <= '9') value |= ch - '0';
      else if (ch >= 'a' && ch <= 'f') value |= ch - 'a' + 10;
      else if (ch >= 'A' && ch <= 'F') value |= ch - 'A' + 10;
      else throw Error("invalid JSON Unicode escape");
    }
    return value;
  }
  std::string parseString() {
    whitespace();
    if (position_ >= input_.size() || input_[position_++] != '"') throw Error("expected JSON string");
    std::string out;
    while (position_ < input_.size()) {
      unsigned char ch = static_cast<unsigned char>(input_[position_++]);
      if (ch == '"') {
        (void)fromUtf8(out);
        return out;
      }
      if (ch < 0x20) throw Error("control character in JSON string");
      if (ch != '\\') { out.push_back(static_cast<char>(ch)); continue; }
      if (position_ == input_.size()) throw Error("truncated JSON escape");
      char escaped = input_[position_++];
      switch (escaped) {
        case '"': out.push_back('"'); break;
        case '\\': out.push_back('\\'); break;
        case '/': out.push_back('/'); break;
        case 'b': out.push_back('\b'); break;
        case 'f': out.push_back('\f'); break;
        case 'n': out.push_back('\n'); break;
        case 'r': out.push_back('\r'); break;
        case 't': out.push_back('\t'); break;
        case 'u': {
          std::uint32_t code = hex4();
          if (code >= 0xd800 && code <= 0xdbff) {
            if (position_ + 2 > input_.size() || input_[position_] != '\\' || input_[position_ + 1] != 'u') {
              throw Error("unpaired high surrogate in JSON");
            }
            position_ += 2;
            std::uint32_t low = hex4();
            if (low < 0xdc00 || low > 0xdfff) throw Error("unpaired high surrogate in JSON");
            code = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
          } else if (code >= 0xdc00 && code <= 0xdfff) {
            throw Error("unpaired low surrogate in JSON");
          }
          appendCodePoint(out, code);
          break;
        }
        default: throw Error("invalid JSON escape");
      }
    }
    throw Error("unterminated JSON string");
  }
  JsonValue parseValue() {
    whitespace();
    if (position_ == input_.size()) throw Error("truncated JSON");
    if (input_[position_] == '"') {
      JsonValue value; value.type = JsonValue::Type::String; value.string = parseString(); return value;
    }
    if (take('{')) {
      JsonValue value; value.type = JsonValue::Type::Object;
      if (take('}')) return value;
      do {
        std::string key = parseString();
        if (!take(':')) throw Error("expected ':' in JSON object");
        if (!value.object.emplace(std::move(key), parseValue()).second) throw Error("duplicate JSON object key");
      } while (take(','));
      if (!take('}')) throw Error("expected '}' in JSON object");
      return value;
    }
    if (take('[')) {
      JsonValue value; value.type = JsonValue::Type::Array;
      if (take(']')) return value;
      do { value.array.push_back(parseValue()); } while (take(','));
      if (!take(']')) throw Error("expected ']' in JSON array");
      return value;
    }
    if (input_[position_] == 't') {
      literal("true"); JsonValue value; value.type = JsonValue::Type::Bool; value.boolean = true; return value;
    }
    if (input_[position_] == 'f') {
      literal("false"); JsonValue value; value.type = JsonValue::Type::Bool; return value;
    }
    if (input_[position_] == 'n') {
      literal("null"); return {};
    }
    if (input_[position_] < '0' || input_[position_] > '9') throw Error("invalid JSON number");
    std::uint64_t number = 0;
    if (input_[position_] == '0' && position_ + 1 < input_.size() &&
        input_[position_ + 1] >= '0' && input_[position_ + 1] <= '9') throw Error("leading zero in JSON number");
    while (position_ < input_.size() && input_[position_] >= '0' && input_[position_] <= '9') {
      unsigned digit = input_[position_++] - '0';
      if (number > (UINT64_MAX - digit) / 10) throw Error("JSON integer overflow");
      number = number * 10 + digit;
    }
    if (position_ < input_.size() && (input_[position_] == '.' || input_[position_] == 'e' ||
        input_[position_] == 'E')) throw Error("JSON numbers must be unsigned integers");
    JsonValue value; value.type = JsonValue::Type::Number; value.number = number; return value;
  }
  std::string_view input_;
  std::size_t position_ = 0;
};

const JsonValue& member(const JsonValue& object, const char* key, JsonValue::Type type) {
  if (object.type != JsonValue::Type::Object) throw Error("expected JSON object");
  auto found = object.object.find(key);
  if (found == object.object.end() || found->second.type != type) {
    throw Error(std::string("missing or invalid JSON field: ") + key);
  }
  return found->second;
}

struct NormalPath {
  std::wstring display;
  std::wstring canonical;
  std::string utf8;
  bool directory = false;
};

bool reservedDevice(std::wstring segment) {
  std::size_t dot = segment.find(L'.');
  if (dot != std::wstring::npos) segment.resize(dot);
  segment = lower(segment);
  if (segment == L"con" || segment == L"prn" || segment == L"aux" || segment == L"nul") return true;
  if (segment.size() == 4 && (segment.rfind(L"com", 0) == 0 || segment.rfind(L"lpt", 0) == 0) &&
      segment[3] >= L'1' && segment[3] <= L'9') return true;
  return false;
}

NormalPath normalizeArchivePath(std::string_view raw, bool permitDirectory) {
  std::wstring value = fromUtf8(raw);
  std::replace(value.begin(), value.end(), L'\\', L'/');
  if (value.empty() || value.front() == L'/' ||
      (value.size() >= 2 && value[0] == L'/' && value[1] == L'/') ||
      (value.size() >= 2 && std::iswalpha(value[0]) && value[1] == L':')) {
    throw Error("absolute, drive, and UNC ZIP paths are forbidden");
  }
  bool directory = value.back() == L'/';
  if (directory) value.pop_back();
  if (value.empty() || (directory && !permitDirectory)) throw Error("invalid ZIP entry path");
  std::size_t start = 0;
  while (start <= value.size()) {
    std::size_t slash = value.find(L'/', start);
    std::wstring segment = value.substr(start, slash == std::wstring::npos ? value.size() - start : slash - start);
    if (segment.empty() || segment == L"." || segment == L".." || segment.find(L':') != std::wstring::npos ||
        segment.back() == L'.' || segment.back() == L' ' || reservedDevice(segment)) {
      throw Error("unsafe ZIP entry path component");
    }
    for (wchar_t ch : segment) if (ch < 0x20) throw Error("control character in ZIP entry path");
    if (slash == std::wstring::npos) break;
    start = slash + 1;
  }
  std::string normalized = toUtf8(value);
  return {value, lower(value), normalized, directory};
}

std::array<std::uint8_t, 32> parseSha256(std::string value) {
  if (value.size() != 64) throw Error("manifest SHA-256 must contain 64 hexadecimal characters");
  std::array<std::uint8_t, 32> out{};
  auto nibble = [](char ch) -> int {
    if (ch >= '0' && ch <= '9') return ch - '0';
    if (ch >= 'a' && ch <= 'f') return ch - 'a' + 10;
    if (ch >= 'A' && ch <= 'F') return ch - 'A' + 10;
    return -1;
  };
  for (std::size_t i = 0; i < out.size(); ++i) {
    int high = nibble(value[i * 2]), lowNibble = nibble(value[i * 2 + 1]);
    if (high < 0 || lowNibble < 0) throw Error("invalid manifest SHA-256");
    out[i] = static_cast<std::uint8_t>((high << 4) | lowNibble);
  }
  return out;
}

struct ManifestFile {
  NormalPath path;
  std::uint64_t size;
  std::array<std::uint8_t, 32> sha256;
  bool seen = false;
};
struct Manifest {
  std::vector<ManifestFile> files;
  std::unordered_map<std::wstring, std::size_t> byPath;
  std::set<std::wstring> directories;
  std::uint64_t totalSize = 0;
};

Manifest parseManifest(const std::vector<std::uint8_t>& bytes) {
  JsonValue root = JsonParser(std::string_view(reinterpret_cast<const char*>(bytes.data()), bytes.size())).parse();
  if (root.type != JsonValue::Type::Object || root.object.size() != 1 || !root.object.contains("files")) {
    throw Error("extract manifest must be exactly {files:[...]}");
  }
  const JsonValue& files = member(root, "files", JsonValue::Type::Array);
  if (files.array.size() > kMaxZipEntries) throw Error("too many manifest files");
  Manifest out;
  out.files.reserve(files.array.size());
  for (const JsonValue& item : files.array) {
    if (item.type != JsonValue::Type::Object || item.object.size() != 3 ||
        !item.object.contains("path") || !item.object.contains("size") || !item.object.contains("sha256")) {
      throw Error("manifest file must contain exactly path, size, and sha256");
    }
    NormalPath path = normalizeArchivePath(member(item, "path", JsonValue::Type::String).string, false);
    std::uint64_t size = member(item, "size", JsonValue::Type::Number).number;
    if (size > kMaxZipBytes || out.totalSize > kMaxZipBytes - size) throw Error("manifest extraction size exceeds limit");
    auto hash = parseSha256(member(item, "sha256", JsonValue::Type::String).string);
    std::size_t index = out.files.size();
    if (!out.byPath.emplace(path.canonical, index).second) throw Error("duplicate manifest path");
    std::size_t slash = path.canonical.find(L'/');
    while (slash != std::wstring::npos) {
      out.directories.insert(path.canonical.substr(0, slash));
      slash = path.canonical.find(L'/', slash + 1);
    }
    out.totalSize += size;
    out.files.push_back({std::move(path), size, hash, false});
  }
  return out;
}

class MappedFile final {
 public:
  explicit MappedFile(const std::wstring& path) {
    file_.reset(CreateFileW(path.c_str(), GENERIC_READ, FILE_SHARE_READ, nullptr, OPEN_EXISTING,
        FILE_ATTRIBUTE_NORMAL, nullptr));
    if (!file_) failWin("CreateFileW(ZIP)");
    LARGE_INTEGER length{};
    if (!GetFileSizeEx(file_.get(), &length)) failWin("GetFileSizeEx(ZIP)");
    if (length.QuadPart < 22 || static_cast<std::uint64_t>(length.QuadPart) > kMaxZipBytes * 2) {
      throw Error("invalid or oversized ZIP");
    }
    size_ = static_cast<std::uint64_t>(length.QuadPart);
    mapping_.reset(CreateFileMappingW(file_.get(), nullptr, PAGE_READONLY, 0, 0, nullptr));
    if (!mapping_) failWin("CreateFileMappingW");
    data_ = static_cast<const std::uint8_t*>(MapViewOfFile(mapping_.get(), FILE_MAP_READ, 0, 0, 0));
    if (!data_) failWin("MapViewOfFile");
  }
  ~MappedFile() { if (data_) UnmapViewOfFile(data_); }
  MappedFile(const MappedFile&) = delete;
  MappedFile& operator=(const MappedFile&) = delete;
  std::uint64_t size() const { return size_; }
  const std::uint8_t* at(std::uint64_t offset, std::uint64_t length) const {
    if (offset > size_ || length > size_ - offset) throw Error("ZIP structure exceeds archive bounds");
    return data_ + static_cast<std::size_t>(offset);
  }
  std::uint16_t u16(std::uint64_t offset) const {
    auto* p = at(offset, 2); return std::uint16_t(p[0]) | (std::uint16_t(p[1]) << 8);
  }
  std::uint32_t u32(std::uint64_t offset) const {
    auto* p = at(offset, 4);
    return std::uint32_t(p[0]) | (std::uint32_t(p[1]) << 8) |
        (std::uint32_t(p[2]) << 16) | (std::uint32_t(p[3]) << 24);
  }
  std::uint64_t u64(std::uint64_t offset) const {
    return std::uint64_t(u32(offset)) | (std::uint64_t(u32(offset + 4)) << 32);
  }
 private:
  Handle file_;
  Handle mapping_;
  const std::uint8_t* data_ = nullptr;
  std::uint64_t size_ = 0;
};

std::string zipName(const std::uint8_t* bytes, std::size_t length, bool isUtf8) {
  std::string raw(reinterpret_cast<const char*>(bytes), length);
  if (raw.find('\0') != std::string::npos) throw Error("NUL in ZIP entry name");
  if (isUtf8) {
    (void)fromUtf8(raw);
    return raw;
  }
  if (length > static_cast<std::size_t>(INT_MAX)) throw Error("ZIP entry name is too long");
  int n = MultiByteToWideChar(437, 0, raw.data(), static_cast<int>(raw.size()), nullptr, 0);
  if (!n) failWin("MultiByteToWideChar(CP437)");
  std::wstring decoded(static_cast<std::size_t>(n), L'\0');
  if (!MultiByteToWideChar(437, 0, raw.data(), static_cast<int>(raw.size()), decoded.data(), n)) {
    failWin("MultiByteToWideChar(CP437)");
  }
  return toUtf8(decoded);
}

struct ZipEntry {
  NormalPath path;
  std::uint64_t compressed = 0;
  std::uint64_t uncompressed = 0;
  std::uint64_t localOffset = 0;
  std::uint64_t dataOffset = 0;
  std::uint16_t method = 0;
  std::uint16_t flags = 0;
};

void zip64Values(const MappedFile& zip, std::uint64_t extraOffset, std::uint16_t extraLength,
    bool needUncompressed, bool needCompressed, bool needOffset,
    std::uint64_t& uncompressed, std::uint64_t& compressed, std::uint64_t& localOffset) {
  std::uint64_t cursor = extraOffset;
  std::uint64_t end = extraOffset + extraLength;
  while (cursor + 4 <= end) {
    std::uint16_t id = zip.u16(cursor);
    std::uint16_t length = zip.u16(cursor + 2);
    cursor += 4;
    if (cursor + length > end) throw Error("truncated ZIP extra field");
    if (id == 0x0001) {
      std::uint64_t valueCursor = cursor;
      auto next = [&]() {
        if (valueCursor + 8 > cursor + length) throw Error("truncated ZIP64 extra field");
        std::uint64_t value = zip.u64(valueCursor);
        valueCursor += 8;
        return value;
      };
      if (needUncompressed) uncompressed = next();
      if (needCompressed) compressed = next();
      if (needOffset) localOffset = next();
      return;
    }
    cursor += length;
  }
  if (needUncompressed || needCompressed || needOffset) throw Error("missing ZIP64 extra field");
}

std::vector<ZipEntry> inspectZip(const MappedFile& zip, Manifest& manifest) {
  std::uint64_t searchStart = zip.size() > 65557 ? zip.size() - 65557 : 0;
  std::optional<std::uint64_t> eocd;
  for (std::uint64_t cursor = zip.size() - 22;; --cursor) {
    if (zip.u32(cursor) == 0x06054b50 && cursor + 22 + zip.u16(cursor + 20) == zip.size()) {
      eocd = cursor;
      break;
    }
    if (cursor == searchStart) break;
  }
  if (!eocd) throw Error("ZIP end-of-central-directory record not found");
  std::uint64_t record = *eocd;
  if (zip.u16(record + 4) != 0 || zip.u16(record + 6) != 0) throw Error("multi-disk ZIP archives are forbidden");
  std::uint64_t entries = zip.u16(record + 10);
  std::uint64_t centralSize = zip.u32(record + 12);
  std::uint64_t centralOffset = zip.u32(record + 16);
  if (entries == 0xffff || centralSize == 0xffffffff || centralOffset == 0xffffffff) {
    if (record < 20 || zip.u32(record - 20) != 0x07064b50) throw Error("missing ZIP64 locator");
    if (zip.u32(record - 16) != 0 || zip.u32(record - 4) != 1) throw Error("multi-disk ZIP64 archives are forbidden");
    std::uint64_t zip64 = zip.u64(record - 12);
    if (zip.u32(zip64) != 0x06064b50 || zip.u32(zip64 + 16) != 0 || zip.u32(zip64 + 20) != 0) {
      throw Error("invalid ZIP64 end record");
    }
    entries = zip.u64(zip64 + 32);
    if (zip.u64(zip64 + 24) != entries) throw Error("inconsistent ZIP64 entry count");
    centralSize = zip.u64(zip64 + 40);
    centralOffset = zip.u64(zip64 + 48);
  } else if (zip.u16(record + 8) != entries) {
    throw Error("inconsistent ZIP entry count");
  }
  if (entries > kMaxZipEntries || centralOffset > zip.size() || centralSize > zip.size() - centralOffset ||
      centralOffset + centralSize > record) throw Error("invalid ZIP central directory bounds");

  std::vector<ZipEntry> result;
  result.reserve(static_cast<std::size_t>(entries));
  std::set<std::wstring> names;
  std::vector<std::pair<std::uint64_t, std::uint64_t>> dataRanges;
  std::uint64_t cursor = centralOffset;
  std::size_t fileCount = 0;
  for (std::uint64_t index = 0; index < entries; ++index) {
    if (zip.u32(cursor) != 0x02014b50) throw Error("invalid ZIP central directory entry");
    std::uint16_t madeBy = zip.u16(cursor + 4);
    std::uint16_t flags = zip.u16(cursor + 8);
    std::uint16_t method = zip.u16(cursor + 10);
    std::uint32_t compressed32 = zip.u32(cursor + 20);
    std::uint32_t uncompressed32 = zip.u32(cursor + 24);
    std::uint16_t nameLength = zip.u16(cursor + 28);
    std::uint16_t extraLength = zip.u16(cursor + 30);
    std::uint16_t commentLength = zip.u16(cursor + 32);
    std::uint16_t disk = zip.u16(cursor + 34);
    std::uint32_t external = zip.u32(cursor + 38);
    std::uint32_t local32 = zip.u32(cursor + 42);
    std::uint64_t next = cursor + 46ULL + nameLength + extraLength + commentLength;
    if (next > centralOffset + centralSize) throw Error("truncated ZIP central directory entry");
    if (flags & 0x2061) throw Error("encrypted, patched, or strong-encryption ZIP entries are forbidden");
    if (method != 0 && method != 8) throw Error("unsupported ZIP compression method");
    if (disk != 0 && disk != 0xffff) throw Error("multi-disk ZIP entry is forbidden");
    std::string rawName = zipName(zip.at(cursor + 46, nameLength), nameLength, (flags & 0x0800) != 0);
    NormalPath path = normalizeArchivePath(rawName, true);
    if (!names.insert(path.canonical).second) throw Error("duplicate ZIP entry path");
    std::uint32_t unixMode = (madeBy >> 8) == 3 ? external >> 16 : 0;
    std::uint32_t unixType = unixMode & 0170000;
    if (unixType == 0120000 || (external & FILE_ATTRIBUTE_REPARSE_POINT)) {
      throw Error("ZIP symlink or reparse entry is forbidden");
    }
    if (unixType && unixType != 0100000 && unixType != 0040000) throw Error("non-file ZIP entry is forbidden");
    bool attrDirectory = unixType == 0040000 || (external & FILE_ATTRIBUTE_DIRECTORY);
    if (attrDirectory != path.directory) throw Error("inconsistent ZIP directory attributes");
    std::uint64_t compressed = compressed32;
    std::uint64_t uncompressed = uncompressed32;
    std::uint64_t localOffset = local32;
    zip64Values(zip, cursor + 46 + nameLength, extraLength,
        uncompressed32 == 0xffffffff, compressed32 == 0xffffffff, local32 == 0xffffffff,
        uncompressed, compressed, localOffset);
    if (compressed > zip.size() || uncompressed > kMaxZipBytes) throw Error("oversized ZIP entry");
    if (zip.u32(localOffset) != 0x04034b50) throw Error("invalid ZIP local header");
    std::uint16_t localFlags = zip.u16(localOffset + 6);
    std::uint16_t localMethod = zip.u16(localOffset + 8);
    std::uint16_t localNameLength = zip.u16(localOffset + 26);
    std::uint16_t localExtraLength = zip.u16(localOffset + 28);
    std::string localName = zipName(zip.at(localOffset + 30, localNameLength), localNameLength,
        (localFlags & 0x0800) != 0);
    NormalPath normalizedLocal = normalizeArchivePath(localName, true);
    if (normalizedLocal.canonical != path.canonical || localFlags != flags || localMethod != method) {
      throw Error("ZIP local and central headers disagree");
    }
    std::uint64_t dataOffset = localOffset + 30ULL + localNameLength + localExtraLength;
    zip.at(dataOffset, compressed);
    if (compressed) dataRanges.emplace_back(dataOffset, dataOffset + compressed);
    if (path.directory) {
      if (compressed || uncompressed || !manifest.directories.contains(path.canonical)) {
        throw Error("unexpected or non-empty ZIP directory entry");
      }
    } else {
      auto found = manifest.byPath.find(path.canonical);
      if (found == manifest.byPath.end()) throw Error("ZIP contains a file absent from manifest");
      if (manifest.files[found->second].size != uncompressed) throw Error("ZIP size disagrees with manifest");
      ++fileCount;
    }
    result.push_back({std::move(path), compressed, uncompressed, localOffset, dataOffset, method, flags});
    cursor = next;
  }
  if (cursor != centralOffset + centralSize || fileCount != manifest.files.size()) {
    throw Error("ZIP and manifest file sets differ");
  }
  std::sort(dataRanges.begin(), dataRanges.end());
  for (std::size_t i = 1; i < dataRanges.size(); ++i) {
    if (dataRanges[i].first < dataRanges[i - 1].second) throw Error("overlapping ZIP entry data");
  }
  return result;
}

std::wstring fullPath(const std::wstring& input) {
  DWORD needed = GetFullPathNameW(input.c_str(), 0, nullptr, nullptr);
  if (!needed) failWin("GetFullPathNameW");
  std::wstring out(needed, L'\0');
  DWORD written = GetFullPathNameW(input.c_str(), needed, out.data(), nullptr);
  if (!written || written >= needed) failWin("GetFullPathNameW");
  out.resize(written);
  return out;
}

std::wstring parentPath(const std::wstring& path) {
  std::size_t slash = path.find_last_of(L"\\/");
  if (slash == std::wstring::npos || slash < 2) throw Error("destination must have an existing parent");
  return path.substr(0, slash);
}

void ensureSafeParent(const std::wstring& parent) {
  if (parent.size() < 3 || !std::iswalpha(parent[0]) || parent[1] != L':' ||
      (parent[2] != L'\\' && parent[2] != L'/')) {
    throw Error("ZIP destination must be on a local drive");
  }
  std::wstring cursor = parent.substr(0, 3);
  for (std::size_t position = 3;;) {
    std::size_t slash = parent.find_first_of(L"\\/", position);
    cursor = slash == std::wstring::npos ? parent : parent.substr(0, slash);
    DWORD attributes = GetFileAttributesW(cursor.c_str());
    if (attributes == INVALID_FILE_ATTRIBUTES) failWin("GetFileAttributesW(destination parent)");
    if (!(attributes & FILE_ATTRIBUTE_DIRECTORY) || (attributes & FILE_ATTRIBUTE_REPARSE_POINT)) {
      throw Error("ZIP destination parent contains a non-directory or reparse point");
    }
    if (slash == std::wstring::npos) break;
    position = slash + 1;
  }
}

std::wstring randomStage(const std::wstring& destination) {
  std::array<std::uint8_t, 16> random{};
  if (BCryptGenRandom(nullptr, random.data(), static_cast<ULONG>(random.size()),
      BCRYPT_USE_SYSTEM_PREFERRED_RNG) < 0) throw Error("BCryptGenRandom failed");
  static constexpr wchar_t digits[] = L"0123456789abcdef";
  std::wstring suffix;
  suffix.reserve(32);
  for (std::uint8_t byte : random) {
    suffix.push_back(digits[byte >> 4]);
    suffix.push_back(digits[byte & 15]);
  }
  return destination + L".roost-stage-" + suffix;
}

void removeTree(const std::wstring& root) noexcept {
  WIN32_FIND_DATAW data{};
  HANDLE raw = FindFirstFileW((root + L"\\*").c_str(), &data);
  if (raw != INVALID_HANDLE_VALUE) {
    Handle search(raw);
    do {
      if (!std::wcscmp(data.cFileName, L".") || !std::wcscmp(data.cFileName, L"..")) continue;
      std::wstring path = root + L"\\" + data.cFileName;
      if ((data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) &&
          !(data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT)) {
        removeTree(path);
      } else {
        SetFileAttributesW(path.c_str(), FILE_ATTRIBUTE_NORMAL);
        if (data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) RemoveDirectoryW(path.c_str());
        else DeleteFileW(path.c_str());
      }
    } while (FindNextFileW(search.get(), &data));
  }
  RemoveDirectoryW(root.c_str());
}

class StageGuard final {
 public:
  explicit StageGuard(std::wstring path) : path_(std::move(path)) {}
  ~StageGuard() { if (active_) removeTree(path_); }
  void release() { active_ = false; }
 private:
  std::wstring path_;
  bool active_ = true;
};

class Sha256 final {
 public:
  Sha256() {
    if (BCryptOpenAlgorithmProvider(&algorithm_, BCRYPT_SHA256_ALGORITHM, nullptr, 0) < 0) {
      throw Error("BCryptOpenAlgorithmProvider(SHA-256) failed");
    }
    DWORD bytes = 0, result = 0;
    if (BCryptGetProperty(algorithm_, BCRYPT_OBJECT_LENGTH, reinterpret_cast<PUCHAR>(&bytes),
        sizeof(bytes), &result, 0) < 0) throw Error("BCryptGetProperty failed");
    object_.resize(bytes);
    if (BCryptCreateHash(algorithm_, &hash_, object_.data(), static_cast<ULONG>(object_.size()),
        nullptr, 0, 0) < 0) throw Error("BCryptCreateHash failed");
  }
  ~Sha256() {
    if (hash_) BCryptDestroyHash(hash_);
    if (algorithm_) BCryptCloseAlgorithmProvider(algorithm_, 0);
  }
  void update(const std::uint8_t* bytes, std::size_t size) {
    while (size) {
      ULONG chunk = static_cast<ULONG>(std::min<std::size_t>(size, 1U << 30));
      if (BCryptHashData(hash_, const_cast<PUCHAR>(bytes), chunk, 0) < 0) throw Error("BCryptHashData failed");
      bytes += chunk;
      size -= chunk;
    }
  }
  std::array<std::uint8_t, 32> finish() {
    std::array<std::uint8_t, 32> out{};
    if (BCryptFinishHash(hash_, out.data(), static_cast<ULONG>(out.size()), 0) < 0) {
      throw Error("BCryptFinishHash failed");
    }
    return out;
  }
 private:
  BCRYPT_ALG_HANDLE algorithm_ = nullptr;
  BCRYPT_HASH_HANDLE hash_ = nullptr;
  std::vector<std::uint8_t> object_;
};

std::array<std::uint8_t, 32> hashAndFlush(const std::wstring& path, std::uint64_t expectedSize) {
  Handle file(CreateFileW(path.c_str(), GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ,
      nullptr, OPEN_EXISTING, FILE_FLAG_SEQUENTIAL_SCAN | FILE_FLAG_OPEN_REPARSE_POINT, nullptr));
  if (!file) failWin("CreateFileW(extracted file)");
  FILE_ATTRIBUTE_TAG_INFO tag{};
  if (!GetFileInformationByHandleEx(file.get(), FileAttributeTagInfo, &tag, sizeof(tag))) {
    failWin("GetFileInformationByHandleEx");
  }
  if (tag.FileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)) {
    throw Error("extracted path is not a regular file");
  }
  BY_HANDLE_FILE_INFORMATION info{};
  if (!GetFileInformationByHandle(file.get(), &info)) failWin("GetFileInformationByHandle");
  if (info.nNumberOfLinks != 1) throw Error("hard-linked extracted file is forbidden");
  Sha256 hash;
  std::array<std::uint8_t, 64 * 1024> buffer{};
  std::uint64_t total = 0;
  for (;;) {
    DWORD got = 0;
    if (!ReadFile(file.get(), buffer.data(), static_cast<DWORD>(buffer.size()), &got, nullptr)) failWin("ReadFile(extracted)");
    if (!got) break;
    if (total > expectedSize - std::min<std::uint64_t>(expectedSize, got)) {
      throw Error("extracted file exceeds manifest size");
    }
    total += got;
    if (total > expectedSize) throw Error("extracted file exceeds manifest size");
    hash.update(buffer.data(), got);
  }
  if (total != expectedSize) throw Error("extracted file size differs from manifest");
  if (!FlushFileBuffers(file.get())) failWin("FlushFileBuffers(extracted)");
  return hash.finish();
}

void scanStage(const std::wstring& root, const std::wstring& relative, Manifest& manifest,
    bool final, std::set<std::wstring>& actual, std::uint64_t& total) {
  std::wstring directory = relative.empty() ? root : root + L"\\" + relative;
  WIN32_FIND_DATAW data{};
  HANDLE raw = FindFirstFileW((directory + L"\\*").c_str(), &data);
  if (raw == INVALID_HANDLE_VALUE) failWin("FindFirstFileW(staging)");
  Handle search(raw);
  do {
    if (!std::wcscmp(data.cFileName, L".") || !std::wcscmp(data.cFileName, L"..")) continue;
    std::wstring childRelative = relative.empty() ? std::wstring(data.cFileName)
                                                  : relative + L"\\" + data.cFileName;
    std::wstring slashName = childRelative;
    std::replace(slashName.begin(), slashName.end(), L'\\', L'/');
    NormalPath normalized = normalizeArchivePath(toUtf8(slashName) +
        ((data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) ? "/" : ""), true);
    if (data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) {
      throw Error("reparse point created during ZIP extraction");
    }
    if (data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) {
      if (!manifest.directories.contains(normalized.canonical)) throw Error("unexpected extracted directory");
      scanStage(root, childRelative, manifest, final, actual, total);
      continue;
    }
    auto found = manifest.byPath.find(normalized.canonical);
    if (found == manifest.byPath.end()) throw Error("unexpected extracted file");
    if (!actual.insert(normalized.canonical).second) throw Error("duplicate extracted file");
    ManifestFile& expectedFile = manifest.files[found->second];
    std::uint64_t size = (std::uint64_t(data.nFileSizeHigh) << 32) | data.nFileSizeLow;
    if (size > expectedFile.size || total > manifest.totalSize - std::min(manifest.totalSize, size)) {
      throw Error("extracted data exceeds manifest bounds");
    }
    total += size;
    if (total > manifest.totalSize) throw Error("extracted data exceeds manifest total");
    if (final) {
      if (size != expectedFile.size) throw Error("extracted file size differs from manifest");
      std::wstring path = root + L"\\" + childRelative;
      if (hashAndFlush(path, expectedFile.size) != expectedFile.sha256) {
        throw Error("extracted file SHA-256 differs from manifest");
      }
      expectedFile.seen = true;
    }
  } while (FindNextFileW(search.get(), &data));
  if (GetLastError() != ERROR_NO_MORE_FILES) failWin("FindNextFileW(staging)");
}

std::wstring quoteArg(const std::wstring& argument) {
  if (!argument.empty() && argument.find_first_of(L" \t\n\v\"") == std::wstring::npos) return argument;
  std::wstring out(1, L'"');
  std::size_t slashes = 0;
  for (wchar_t ch : argument) {
    if (ch == L'\\') { ++slashes; continue; }
    if (ch == L'"') {
      out.append(slashes * 2 + 1, L'\\');
      out.push_back(L'"');
      slashes = 0;
      continue;
    }
    out.append(slashes, L'\\');
    slashes = 0;
    out.push_back(ch);
  }
  out.append(slashes * 2, L'\\');
  out.push_back(L'"');
  return out;
}

void runTar(const std::wstring& archive, const std::wstring& stage, Manifest& manifest) {
  wchar_t system[MAX_PATH]{};
  UINT n = GetSystemDirectoryW(system, MAX_PATH);
  if (!n || n >= MAX_PATH) failWin("GetSystemDirectoryW");
  std::wstring executable = std::wstring(system) + L"\\tar.exe";
  if (GetFileAttributesW(executable.c_str()) == INVALID_FILE_ATTRIBUTES) throw Error("inbox Windows tar.exe is unavailable");
  std::wstring command = quoteArg(executable) + L" -xf " + quoteArg(archive) + L" -C " + quoteArg(stage);
  std::vector<wchar_t> mutableCommand(command.begin(), command.end());
  mutableCommand.push_back(L'\0');
  Handle nullFile(CreateFileW(L"NUL", GENERIC_READ | GENERIC_WRITE,
      FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr));
  if (!nullFile) failWin("CreateFileW(NUL)");
  SetHandleInformation(nullFile.get(), HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
  STARTUPINFOW startup{};
  startup.cb = sizeof(startup);
  startup.dwFlags = STARTF_USESTDHANDLES;
  startup.hStdInput = nullFile.get();
  startup.hStdOutput = nullFile.get();
  startup.hStdError = nullFile.get();
  PROCESS_INFORMATION process{};
  if (!CreateProcessW(executable.c_str(), mutableCommand.data(), nullptr, nullptr, TRUE,
      CREATE_NO_WINDOW | CREATE_SUSPENDED, nullptr, stage.c_str(), &startup, &process)) {
    failWin("CreateProcessW(tar.exe)");
  }
  Handle processHandle(process.hProcess);
  Handle threadHandle(process.hThread);
  Handle job(CreateJobObjectW(nullptr, nullptr));
  if (!job) failWin("CreateJobObjectW(tar)");
  JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
  limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
  if (!SetInformationJobObject(job.get(), JobObjectExtendedLimitInformation, &limits, sizeof(limits))) {
    failWin("SetInformationJobObject(tar)");
  }
  if (!AssignProcessToJobObject(job.get(), processHandle.get())) failWin("AssignProcessToJobObject(tar)");
  if (ResumeThread(threadHandle.get()) == static_cast<DWORD>(-1)) failWin("ResumeThread(tar)");
  auto deadline = std::chrono::steady_clock::now() + std::chrono::minutes(15);
  for (;;) {
    DWORD wait = WaitForSingleObject(processHandle.get(), 10);
    if (wait == WAIT_OBJECT_0) break;
    if (wait != WAIT_TIMEOUT) failWin("WaitForSingleObject(tar)");
    try {
      std::set<std::wstring> actual;
      std::uint64_t total = 0;
      scanStage(stage, L"", manifest, false, actual, total);
    } catch (...) {
      TerminateJobObject(job.get(), ERROR_INVALID_DATA);
      WaitForSingleObject(processHandle.get(), INFINITE);
      throw;
    }
    if (std::chrono::steady_clock::now() >= deadline) {
      TerminateJobObject(job.get(), ERROR_TIMEOUT);
      WaitForSingleObject(processHandle.get(), INFINITE);
      throw Error("ZIP extraction timed out");
    }
  }
  DWORD exitCode = 0;
  if (!GetExitCodeProcess(processHandle.get(), &exitCode)) failWin("GetExitCodeProcess(tar)");
  if (exitCode != 0) throw Error("tar.exe rejected the ZIP archive [exit=" + std::to_string(exitCode) + "]");
}

void extractZip(const std::vector<std::wstring>& args) {
  expect(args, 2, "extract-zip <zip> <destination>");
  std::vector<std::uint8_t> input = framedInput();
  Manifest manifest = parseManifest(input);
  std::wstring archive = fullPath(args[0]);
  std::wstring destination = fullPath(args[1]);
  std::wstring parent = parentPath(destination);
  ensureSafeParent(parent);
  DWORD destinationAttributes = GetFileAttributesW(destination.c_str());
  if (destinationAttributes != INVALID_FILE_ATTRIBUTES ||
      (GetLastError() != ERROR_FILE_NOT_FOUND && GetLastError() != ERROR_PATH_NOT_FOUND)) {
    throw Error("ZIP destination already exists");
  }
  MappedFile zip(archive);
  (void)inspectZip(zip, manifest);
  std::wstring stage = randomStage(destination);
  if (!CreateDirectoryW(stage.c_str(), nullptr)) failWin("CreateDirectoryW(staging)");
  StageGuard guard(stage);
  applyPrivateDacl(stage);
  runTar(archive, stage, manifest);
  std::set<std::wstring> actual;
  std::uint64_t total = 0;
  scanStage(stage, L"", manifest, true, actual, total);
  if (actual.size() != manifest.files.size() || total != manifest.totalSize) {
    throw Error("extracted file set differs from manifest");
  }
  for (const auto& file : manifest.files) if (!file.seen) throw Error("manifest file missing after extraction");
  if (!MoveFileExW(stage.c_str(), destination.c_str(), MOVEFILE_WRITE_THROUGH)) failWin("MoveFileExW(extracted directory)");
  guard.release();
  std::string out = "{\"files\":[";
  for (std::size_t i = 0; i < manifest.files.size(); ++i) {
    if (i) out.push_back(',');
    out += json(manifest.files[i].path.utf8);
  }
  out += "]}";
  emit(std::move(out));
}

bool allowedService(const std::wstring& name) {
  return name == L"RoostKeeperV2" || name == L"RoostWorkerV2" ||
      name == L"RoostCoordinatorV2" || name == L"RoostUpdaterV2";
}

void requireService(const std::wstring& name) {
  if (!allowedService(name)) throw Error("service name is not in the Roost V2 allowlist");
}

ServiceHandle scm(DWORD access = SC_MANAGER_CONNECT) {
  SC_HANDLE value = OpenSCManagerW(nullptr, nullptr, access);
  if (!value) failWin("OpenSCManagerW");
  return ServiceHandle(value);
}

void resolveAccountSid(const std::vector<std::wstring>& args) {
  expect(args, 1, "resolve-account-sid <account>");
  DWORD sidBytes = 0, domainChars = 0;
  SID_NAME_USE use{};
  LookupAccountNameW(nullptr, args[0].c_str(), nullptr, &sidBytes, nullptr, &domainChars, &use);
  if (GetLastError() != ERROR_INSUFFICIENT_BUFFER) failWin("LookupAccountNameW");
  std::vector<std::uint8_t> sid(sidBytes);
  std::wstring domain(domainChars, L'\0');
  if (!LookupAccountNameW(nullptr, args[0].c_str(), sid.data(), &sidBytes,
      domain.data(), &domainChars, &use)) failWin("LookupAccountNameW");
  domain.resize(domainChars);
  DWORD accountChars = 0;
  domainChars = 0;
  LookupAccountSidW(nullptr, sid.data(), nullptr, &accountChars, nullptr, &domainChars, &use);
  if (GetLastError() != ERROR_INSUFFICIENT_BUFFER) failWin("LookupAccountSidW");
  std::wstring account(accountChars, L'\0');
  domain.assign(domainChars, L'\0');
  if (!LookupAccountSidW(nullptr, sid.data(), account.data(), &accountChars,
      domain.data(), &domainChars, &use)) failWin("LookupAccountSidW");
  account.resize(accountChars);
  domain.resize(domainChars);
  std::wstring canonical = domain.empty() ? account : domain + L"\\" + account;
  emit("{\"sid\":" + json(sidText(sid.data())) + ",\"canonicalAccount\":" + json(canonical) + "}");
}

class LsaPolicy final {
 public:
  explicit LsaPolicy(LSA_HANDLE value) : value_(value) {}
  ~LsaPolicy() { if (value_) LsaClose(value_); }
  LSA_HANDLE get() const { return value_; }
 private:
  LSA_HANDLE value_;
};

void grantLogonAsService(const std::vector<std::wstring>& args) {
  expect(args, 1, "grant-logon-as-service <sid>");
  PSID raw = nullptr;
  if (!ConvertStringSidToSidW(args[0].c_str(), &raw)) failWin("ConvertStringSidToSidW");
  Local<void> sid(raw);
  if (!IsValidSid(sid.get())) throw Error("invalid account SID");
  LSA_OBJECT_ATTRIBUTES attributes{};
  attributes.Length = sizeof(attributes);
  LSA_HANDLE policyRaw = nullptr;
  NTSTATUS status = LsaOpenPolicy(nullptr, &attributes,
      POLICY_LOOKUP_NAMES | POLICY_CREATE_ACCOUNT, &policyRaw);
  if (status) failLsa("LsaOpenPolicy", status);
  LsaPolicy policy(policyRaw);
  bool present = false;
  PLSA_UNICODE_STRING rights = nullptr;
  ULONG count = 0;
  status = LsaEnumerateAccountRights(policy.get(), sid.get(), &rights, &count);
  if (status == 0) {
    for (ULONG i = 0; i < count; ++i) {
      std::wstring right(rights[i].Buffer, rights[i].Length / sizeof(wchar_t));
      if (right == L"SeServiceLogonRight") present = true;
    }
    LsaFreeMemory(rights);
  } else if (static_cast<ULONG>(status) != 0xC0000034UL) {
    failLsa("LsaEnumerateAccountRights", status);
  }
  if (!present) {
    std::wstring name = L"SeServiceLogonRight";
    LSA_UNICODE_STRING right{};
    right.Buffer = name.data();
    right.Length = static_cast<USHORT>(name.size() * sizeof(wchar_t));
    right.MaximumLength = right.Length + sizeof(wchar_t);
    status = LsaAddAccountRights(policy.get(), sid.get(), &right, 1);
    if (status) failLsa("LsaAddAccountRights", status);
  }
  emit(std::string("{\"changed\":") + (present ? "false}" : "true}"));
}

std::vector<std::uint8_t> serviceSecurity(SC_HANDLE service) {
  DWORD needed = 0;
  QueryServiceObjectSecurity(service, DACL_SECURITY_INFORMATION, nullptr, 0, &needed);
  if (GetLastError() != ERROR_INSUFFICIENT_BUFFER) failWin("QueryServiceObjectSecurity");
  std::vector<std::uint8_t> out(needed);
  if (!QueryServiceObjectSecurity(service, DACL_SECURITY_INFORMATION,
      reinterpret_cast<PSECURITY_DESCRIPTOR>(out.data()), needed, &needed)) {
    failWin("QueryServiceObjectSecurity");
  }
  return out;
}

void applyServiceDacl(const std::vector<std::wstring>& args) {
  expect(args, 3, "apply-service-dacl <service> <sid> <rights>");
  requireService(args[0]);
  if (args[2] != L"START,STOP,QUERY_STATUS,QUERY_CONFIG,CHANGE_CONFIG") {
    throw Error("service DACL rights are not the approved allowlist");
  }
  PSID rawSid = nullptr;
  if (!ConvertStringSidToSidW(args[1].c_str(), &rawSid)) failWin("ConvertStringSidToSidW");
  Local<void> sid(rawSid);
  if (!IsValidSid(sid.get())) throw Error("invalid service DACL SID");
  ServiceHandle manager = scm();
  ServiceHandle service(OpenServiceW(manager.get(), args[0].c_str(), READ_CONTROL | WRITE_DAC));
  if (!service) failWin("OpenServiceW");
  std::vector<std::uint8_t> security = serviceSecurity(service.get());
  BOOL present = FALSE, defaulted = FALSE;
  PACL existing = nullptr;
  if (!GetSecurityDescriptorDacl(reinterpret_cast<PSECURITY_DESCRIPTOR>(security.data()),
      &present, &existing, &defaulted) || !present) throw Error("service has no DACL");
  EXPLICIT_ACCESSW entry{};
  entry.grfAccessPermissions = SERVICE_START | SERVICE_STOP | SERVICE_QUERY_STATUS |
      SERVICE_QUERY_CONFIG | SERVICE_CHANGE_CONFIG;
  entry.grfAccessMode = GRANT_ACCESS;
  entry.grfInheritance = NO_INHERITANCE;
  entry.Trustee.TrusteeForm = TRUSTEE_IS_SID;
  entry.Trustee.TrusteeType = TRUSTEE_IS_USER;
  entry.Trustee.ptstrName = static_cast<LPWSTR>(sid.get());
  PACL rawAcl = nullptr;
  DWORD code = SetEntriesInAclW(1, &entry, existing, &rawAcl);
  if (code != ERROR_SUCCESS) failWin("SetEntriesInAclW", code);
  Local<ACL> acl(rawAcl);
  SECURITY_DESCRIPTOR descriptor{};
  if (!InitializeSecurityDescriptor(&descriptor, SECURITY_DESCRIPTOR_REVISION) ||
      !SetSecurityDescriptorDacl(&descriptor, TRUE, acl.get(), FALSE)) failWin("SetSecurityDescriptorDacl");
  if (!SetServiceObjectSecurity(service.get(), DACL_SECURITY_INFORMATION, &descriptor)) {
    failWin("SetServiceObjectSecurity");
  }
  security = serviceSecurity(service.get());
  wchar_t* rawText = nullptr;
  if (!ConvertSecurityDescriptorToStringSecurityDescriptorW(
      reinterpret_cast<PSECURITY_DESCRIPTOR>(security.data()), SDDL_REVISION_1,
      DACL_SECURITY_INFORMATION, &rawText, nullptr)) {
    failWin("ConvertSecurityDescriptorToStringSecurityDescriptorW");
  }
  Local<wchar_t> text(rawText);
  emit("{\"sddl\":" + json(std::wstring(text.get())) + "}");
}

void configureServiceAccount(const std::vector<std::wstring>& args) {
  expect(args, 2, "configure-service-account <service> <canonical-account>");
  requireService(args[0]);
  SecureBytes bytes(framedInput(1024 * 1024));
  std::string_view passwordBytes(reinterpret_cast<const char*>(bytes.get().data()), bytes.get().size());
  if (passwordBytes.find('\0') != std::string_view::npos) throw Error("service password contains NUL");
  SecureWide password(fromUtf8(passwordBytes));
  ServiceHandle manager = scm();
  ServiceHandle service(OpenServiceW(manager.get(), args[0].c_str(), SERVICE_CHANGE_CONFIG));
  if (!service) failWin("OpenServiceW");
  if (!ChangeServiceConfigW(service.get(), SERVICE_NO_CHANGE, SERVICE_NO_CHANGE, SERVICE_NO_CHANGE,
      nullptr, nullptr, nullptr, nullptr, args[1].c_str(), password.c_str(), nullptr)) {
    failWin("ChangeServiceConfigW(account)");
  }
  emit("{\"configured\":true,\"changed\":true}");
}

struct ServiceConfigSnapshot {
  std::wstring image;
  std::wstring account;
  std::wstring displayName;
  std::vector<std::wstring> dependencies;
  DWORD startType = 0;
};

ServiceConfigSnapshot queryConfig(SC_HANDLE service) {
  DWORD needed = 0;
  QueryServiceConfigW(service, nullptr, 0, &needed);
  if (GetLastError() != ERROR_INSUFFICIENT_BUFFER) failWin("QueryServiceConfigW");
  std::vector<std::uint8_t> storage(needed);
  auto* config = reinterpret_cast<QUERY_SERVICE_CONFIGW*>(storage.data());
  if (!QueryServiceConfigW(service, config, needed, &needed)) failWin("QueryServiceConfigW");
  ServiceConfigSnapshot out;
  out.image = config->lpBinaryPathName ? config->lpBinaryPathName : L"";
  out.account = config->lpServiceStartName ? config->lpServiceStartName : L"";
  out.startType = config->dwStartType;
  out.displayName = config->lpDisplayName ? config->lpDisplayName : L"";
  if (config->lpDependencies) {
    for (const wchar_t* item = config->lpDependencies; *item; item += std::wcslen(item) + 1) {
      out.dependencies.emplace_back(item);
    }
  }
  return out;
}
std::vector<std::uint8_t> queryServiceConfig2(SC_HANDLE service, DWORD level) {
  DWORD needed = 0;
  QueryServiceConfig2W(service, level, nullptr, 0, &needed);
  if (GetLastError() != ERROR_INSUFFICIENT_BUFFER) failWin("QueryServiceConfig2W");
  std::vector<std::uint8_t> storage(needed);
  if (!QueryServiceConfig2W(service, level, storage.data(), needed, &needed)) {
    failWin("QueryServiceConfig2W");
  }
  return storage;
}

struct ServiceRecoverySnapshot {
  DWORD resetPeriodSeconds = 0;
  std::wstring rebootMessage;
  std::wstring command;
  std::vector<SC_ACTION> actions;
  bool actionsOnNonCrashFailures = false;
};

std::wstring queryServiceDescription(SC_HANDLE service) {
  std::vector<std::uint8_t> storage = queryServiceConfig2(service, SERVICE_CONFIG_DESCRIPTION);
  const auto* description = reinterpret_cast<const SERVICE_DESCRIPTIONW*>(storage.data());
  return description->lpDescription ? description->lpDescription : L"";
}

ServiceRecoverySnapshot queryServiceRecovery(SC_HANDLE service) {
  std::vector<std::uint8_t> actionStorage = queryServiceConfig2(service, SERVICE_CONFIG_FAILURE_ACTIONS);
  const auto* config = reinterpret_cast<const SERVICE_FAILURE_ACTIONSW*>(actionStorage.data());
  ServiceRecoverySnapshot out;
  out.resetPeriodSeconds = config->dwResetPeriod;
  out.rebootMessage = config->lpRebootMsg ? config->lpRebootMsg : L"";
  out.command = config->lpCommand ? config->lpCommand : L"";
  if (config->cActions > 0) {
    if (!config->lpsaActions) throw Error("service recovery actions are missing");
    out.actions.assign(config->lpsaActions, config->lpsaActions + config->cActions);
  }
  std::vector<std::uint8_t> flagStorage = queryServiceConfig2(service, SERVICE_CONFIG_FAILURE_ACTIONS_FLAG);
  const auto* flag = reinterpret_cast<const SERVICE_FAILURE_ACTIONS_FLAG*>(flagStorage.data());
  out.actionsOnNonCrashFailures = flag->fFailureActionsOnNonCrashFailures != FALSE;
  return out;
}

std::wstring queryServiceSddl(SC_HANDLE service) {
  std::vector<std::uint8_t> security = serviceSecurity(service);
  wchar_t* rawText = nullptr;
  if (!ConvertSecurityDescriptorToStringSecurityDescriptorW(
      reinterpret_cast<PSECURITY_DESCRIPTOR>(security.data()), SDDL_REVISION_1,
      DACL_SECURITY_INFORMATION, &rawText, nullptr)) {
    failWin("ConvertSecurityDescriptorToStringSecurityDescriptorW(service)");
  }
  Local<wchar_t> text(rawText);
  return text.get();
}

const char* serviceRecoveryActionName(SC_ACTION_TYPE type) {
  switch (type) {
    case SC_ACTION_NONE: return "none";
    case SC_ACTION_RESTART: return "restart";
    case SC_ACTION_REBOOT: return "reboot";
    case SC_ACTION_RUN_COMMAND: return "run-command";
    default: throw Error("SCM returned an unknown recovery action");
  }
}


std::vector<std::wstring> serviceEnvironment(const std::wstring& name) {
  HKEY raw = nullptr;
  std::wstring path = L"SYSTEM\\CurrentControlSet\\Services\\" + name;
  LONG code = RegOpenKeyExW(HKEY_LOCAL_MACHINE, path.c_str(), 0, KEY_QUERY_VALUE, &raw);
  if (code != ERROR_SUCCESS) failWin("RegOpenKeyExW(service)", code);
  struct KeyGuard { HKEY key; ~KeyGuard() { RegCloseKey(key); } } key{raw};
  DWORD type = 0, bytes = 0;
  code = RegQueryValueExW(raw, L"Environment", nullptr, &type, nullptr, &bytes);
  if (code == ERROR_FILE_NOT_FOUND) return {};
  if (code != ERROR_SUCCESS) failWin("RegQueryValueExW(Environment)", code);
  if (type != REG_MULTI_SZ || bytes % sizeof(wchar_t)) throw Error("service Environment is not REG_MULTI_SZ");
  std::vector<wchar_t> data(bytes / sizeof(wchar_t) + 2, L'\0');
  code = RegQueryValueExW(raw, L"Environment", nullptr, &type,
      reinterpret_cast<BYTE*>(data.data()), &bytes);
  if (code != ERROR_SUCCESS) failWin("RegQueryValueExW(Environment)", code);
  std::vector<std::wstring> out;
  for (const wchar_t* item = data.data(); *item; item += std::wcslen(item) + 1) out.emplace_back(item);
  return out;
}

std::string serviceState(DWORD state) {
  switch (state) {
    case SERVICE_STOPPED: return "stopped";
    case SERVICE_START_PENDING: return "start-pending";
    case SERVICE_STOP_PENDING: return "stop-pending";
    case SERVICE_RUNNING: return "running";
    case SERVICE_CONTINUE_PENDING: return "continue-pending";
    case SERVICE_PAUSE_PENDING: return "pause-pending";
    case SERVICE_PAUSED: return "paused";
    default: throw Error("SCM returned an unknown service state");
  }
}

std::vector<std::wstring> splitCommandLine(const std::wstring& raw) {
  int count = 0;
  LPWSTR* values = CommandLineToArgvW(raw.c_str(), &count);
  if (!values || count <= 0) failWin("CommandLineToArgvW");
  Local<LPWSTR> guard(values);
  std::vector<std::wstring> out;
  out.reserve(count);
  for (int i = 0; i < count; ++i) out.emplace_back(values[i]);
  return out;
}

void queryServiceCommand(const std::vector<std::wstring>& args) {
  expect(args, 1, "service-query <service>");
  requireService(args[0]);
  ServiceHandle manager = scm();
  ServiceHandle service(OpenServiceW(
      manager.get(), args[0].c_str(), SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS | READ_CONTROL));
  if (!service) failWin("OpenServiceW");
  ServiceConfigSnapshot config = queryConfig(service.get());
  SERVICE_STATUS_PROCESS status{};
  DWORD bytes = 0;
  if (!QueryServiceStatusEx(service.get(), SC_STATUS_PROCESS_INFO,
      reinterpret_cast<BYTE*>(&status), sizeof(status), &bytes)) failWin("QueryServiceStatusEx");
  std::vector<std::wstring> argv = splitCommandLine(config.image);
  std::vector<std::wstring> environment = serviceEnvironment(args[0]);
  const std::wstring description = queryServiceDescription(service.get());
  const ServiceRecoverySnapshot recovery = queryServiceRecovery(service.get());
  const std::wstring securityDescriptor = queryServiceSddl(service.get());
  const char* startType = config.startType == SERVICE_AUTO_START ? "automatic" :
      config.startType == SERVICE_DEMAND_START ? "manual" :
      config.startType == SERVICE_DISABLED ? "disabled" : nullptr;
  if (!startType) throw Error("unsupported service start type");
  std::string out = "{\"name\":" + json(args[0]) + ",\"state\":" + json(serviceState(status.dwCurrentState)) +
      ",\"pid\":" + std::to_string(status.dwProcessId) + ",\"startType\":" + json(startType) +
      ",\"imagePathRaw\":" + json(config.image) + ",\"binaryArgv\":[";
  for (std::size_t i = 0; i < argv.size(); ++i) { if (i) out.push_back(','); out += json(argv[i]); }
  out += "],\"account\":" + json(config.account) + ",\"dependencies\":[";
  for (std::size_t i = 0; i < config.dependencies.size(); ++i) { if (i) out.push_back(','); out += json(config.dependencies[i]); }
  out += "],\"environment\":{";
  bool first = true;
  for (const std::wstring& entry : environment) {
    std::size_t equals = entry.find(L'=');
    if (!equals || equals == std::wstring::npos) throw Error("invalid service Environment entry");
    if (!first) out.push_back(',');
    first = false;
    out += json(entry.substr(0, equals)) + ":" + json(entry.substr(equals + 1));
  }
  out += "},\"displayName\":" + json(config.displayName) +
      ",\"description\":" + json(description) +
      ",\"recoveryPolicy\":{\"resetPeriodSeconds\":" + std::to_string(recovery.resetPeriodSeconds) +
      ",\"rebootMessage\":" + json(recovery.rebootMessage) +
      ",\"command\":" + json(recovery.command) + ",\"actions\":[";
  for (std::size_t i = 0; i < recovery.actions.size(); ++i) {
    if (i) out.push_back(',');
    out += "{\"type\":" + json(serviceRecoveryActionName(recovery.actions[i].Type)) +
        ",\"delayMs\":" + std::to_string(recovery.actions[i].Delay) + "}";
  }
  out += "],\"actionsOnNonCrashFailures\":" +
      std::string(recovery.actionsOnNonCrashFailures ? "true" : "false") +
      "},\"securityDescriptor\":" + json(securityDescriptor) + "}";
  emit(std::move(out));
}

std::optional<std::reference_wrapper<const JsonValue>> optionalMember(
    const JsonValue& object, const char* key, JsonValue::Type type) {
  auto found = object.object.find(key);
  if (found == object.object.end()) return std::nullopt;
  if (found->second.type != type) throw Error(std::string("invalid JSON field: ") + key);
  return std::cref(found->second);
}

std::vector<std::wstring> stringArray(const JsonValue& value, const char* label) {
  if (value.type != JsonValue::Type::Array) throw Error(std::string(label) + " must be an array");
  std::vector<std::wstring> out;
  out.reserve(value.array.size());
  for (const JsonValue& item : value.array) {
    if (item.type != JsonValue::Type::String || item.string.find('\0') != std::string::npos) {
      throw Error(std::string(label) + " must contain strings without NUL");
    }
    out.push_back(fromUtf8(item.string));
  }
  return out;
}

std::vector<wchar_t> multiString(const std::vector<std::wstring>& values) {
  std::vector<wchar_t> out;
  for (const std::wstring& value : values) {
    if (value.empty() || value.find(L'\0') != std::wstring::npos) throw Error("empty/NUL MULTI_SZ item");
    out.insert(out.end(), value.begin(), value.end());
    out.push_back(L'\0');
  }
  out.push_back(L'\0');
  if (values.empty()) out.push_back(L'\0');
  return out;
}

void writeServiceEnvironment(const std::wstring& name, const JsonValue& object) {
  std::vector<std::wstring> entries;
  for (const auto& [key, value] : object.object) {
    if (key.empty() || key.find('=') != std::string::npos || key.find('\0') != std::string::npos ||
        value.type != JsonValue::Type::String || value.string.find('\0') != std::string::npos) {
      throw Error("invalid service environment");
    }
    entries.push_back(fromUtf8(key) + L"=" + fromUtf8(value.string));
  }
  std::vector<wchar_t> data = multiString(entries);
  HKEY raw = nullptr;
  std::wstring path = L"SYSTEM\\CurrentControlSet\\Services\\" + name;
  DWORD disposition = 0;
  LONG code = RegCreateKeyExW(HKEY_LOCAL_MACHINE, path.c_str(), 0, nullptr,
      REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, nullptr, &raw, &disposition);
  if (code != ERROR_SUCCESS) failWin("RegCreateKeyExW(service)", code);
  struct KeyGuard { HKEY key; ~KeyGuard() { RegCloseKey(key); } } keyGuard{raw};
  code = RegSetValueExW(raw, L"Environment", 0, REG_MULTI_SZ,
      reinterpret_cast<const BYTE*>(data.data()), static_cast<DWORD>(data.size() * sizeof(wchar_t)));
  if (code != ERROR_SUCCESS) failWin("RegSetValueExW(Environment)", code);
}

void configureServiceCommand(const std::vector<std::wstring>& args) {
  expect(args, 1, "service-config <service>");
  requireService(args[0]);
  std::vector<std::uint8_t> input = framedInput();
  JsonValue root = JsonParser(std::string_view(reinterpret_cast<const char*>(input.data()), input.size())).parse();
  if (root.type != JsonValue::Type::Object || root.object.empty()) {
    throw Error("service config must be a non-empty object");
  }
  static const std::set<std::string> allowed{
      "binaryArgv", "startType", "dependencies", "environment", "displayName",
      "description", "recoveryPolicy", "securityDescriptor"};
  for (const auto& [key, value] : root.object) {
    (void)value;
    if (!allowed.contains(key)) throw Error("unknown service config field: " + key);
  }
  auto argvValue = optionalMember(root, "binaryArgv", JsonValue::Type::Array);
  auto startValue = optionalMember(root, "startType", JsonValue::Type::String);
  auto dependenciesValue = optionalMember(root, "dependencies", JsonValue::Type::Array);
  auto environmentValue = optionalMember(root, "environment", JsonValue::Type::Object);
  auto displayValue = optionalMember(root, "displayName", JsonValue::Type::String);
  auto descriptionValue = optionalMember(root, "description", JsonValue::Type::String);
  auto recoveryValue = optionalMember(root, "recoveryPolicy", JsonValue::Type::Object);
  auto securityValue = optionalMember(root, "securityDescriptor", JsonValue::Type::String);
  std::optional<std::wstring> binary;
  if (argvValue) {
    std::vector<std::wstring> argv = stringArray(argvValue->get(), "binaryArgv");
    if (argv.empty() || argv[0].empty()) throw Error("binaryArgv must not be empty");
    std::wstring command;
    for (std::size_t i = 0; i < argv.size(); ++i) {
      if (i) command.push_back(L' ');
      command += quoteArg(argv[i]);
    }
    binary = std::move(command);
  }
  DWORD startType = SERVICE_NO_CHANGE;
  if (startValue) {
    const std::string& value = startValue->get().string;
    if (value == "automatic") startType = SERVICE_AUTO_START;
    else if (value == "manual") startType = SERVICE_DEMAND_START;
    else if (value == "disabled") startType = SERVICE_DISABLED;
    else throw Error("invalid service startType");
  }
  std::optional<std::vector<wchar_t>> dependencies;
  if (dependenciesValue) {
    dependencies = multiString(stringArray(dependenciesValue->get(), "dependencies"));
  }
  std::optional<std::wstring> displayName;
  if (displayValue) displayName = fromUtf8(displayValue->get().string);
  const bool needsService = binary || startValue || dependencies || displayValue ||
      descriptionValue || recoveryValue || securityValue;
  if (needsService) {
    ServiceHandle manager = scm();
    DWORD access = SERVICE_CHANGE_CONFIG | (securityValue ? WRITE_DAC : 0);
    ServiceHandle service(OpenServiceW(manager.get(), args[0].c_str(), access));
    if (!service) failWin("OpenServiceW");
    if (binary || startValue || dependencies || displayValue) {
      if (!ChangeServiceConfigW(service.get(), SERVICE_NO_CHANGE, startType, SERVICE_NO_CHANGE,
          binary ? binary->c_str() : nullptr, nullptr, nullptr,
          dependencies ? dependencies->data() : nullptr, nullptr, nullptr,
          displayName ? displayName->c_str() : nullptr)) {
        failWin("ChangeServiceConfigW");
      }
    }
    if (descriptionValue) {
      std::wstring description = fromUtf8(descriptionValue->get().string);
      SERVICE_DESCRIPTIONW config{};
      config.lpDescription = description.data();
      if (!ChangeServiceConfig2W(service.get(), SERVICE_CONFIG_DESCRIPTION, &config)) {
        failWin("ChangeServiceConfig2W(description)");
      }
    }
    if (recoveryValue) {
      const JsonValue& policy = recoveryValue->get();
      auto required = [&](const char* key, JsonValue::Type type) -> const JsonValue& {
        auto found = policy.object.find(key);
        if (found == policy.object.end() || found->second.type != type) {
          throw Error(std::string("invalid recoveryPolicy field: ") + key);
        }
        return found->second;
      };
      const std::uint64_t reset = required("resetPeriodSeconds", JsonValue::Type::Number).number;
      if (reset > std::numeric_limits<DWORD>::max()) throw Error("recovery reset period is too large");
      std::wstring rebootMessage = fromUtf8(required("rebootMessage", JsonValue::Type::String).string);
      std::wstring command = fromUtf8(required("command", JsonValue::Type::String).string);
      const JsonValue& actionValues = required("actions", JsonValue::Type::Array);
      std::vector<SC_ACTION> actions;
      actions.reserve(actionValues.array.size());
      for (const JsonValue& actionValue : actionValues.array) {
        if (actionValue.type != JsonValue::Type::Object) throw Error("recovery action must be an object");
        auto type = actionValue.object.find("type");
        auto delay = actionValue.object.find("delayMs");
        if (type == actionValue.object.end() || type->second.type != JsonValue::Type::String ||
            delay == actionValue.object.end() || delay->second.type != JsonValue::Type::Number ||
            delay->second.number > std::numeric_limits<DWORD>::max()) {
          throw Error("invalid recovery action");
        }
        SC_ACTION action{};
        if (type->second.string == "none") action.Type = SC_ACTION_NONE;
        else if (type->second.string == "restart") action.Type = SC_ACTION_RESTART;
        else if (type->second.string == "reboot") action.Type = SC_ACTION_REBOOT;
        else if (type->second.string == "run-command") action.Type = SC_ACTION_RUN_COMMAND;
        else throw Error("unknown recovery action type");
        action.Delay = static_cast<DWORD>(delay->second.number);
        actions.push_back(action);
      }
      SERVICE_FAILURE_ACTIONSW failure{};
      failure.dwResetPeriod = static_cast<DWORD>(reset);
      failure.lpRebootMsg = rebootMessage.data();
      failure.lpCommand = command.data();
      failure.cActions = static_cast<DWORD>(actions.size());
      failure.lpsaActions = actions.empty() ? nullptr : actions.data();
      if (!ChangeServiceConfig2W(service.get(), SERVICE_CONFIG_FAILURE_ACTIONS, &failure)) {
        failWin("ChangeServiceConfig2W(failure-actions)");
      }
      SERVICE_FAILURE_ACTIONS_FLAG flag{};
      flag.fFailureActionsOnNonCrashFailures =
          required("actionsOnNonCrashFailures", JsonValue::Type::Bool).boolean ? TRUE : FALSE;
      if (!ChangeServiceConfig2W(service.get(), SERVICE_CONFIG_FAILURE_ACTIONS_FLAG, &flag)) {
        failWin("ChangeServiceConfig2W(failure-actions-flag)");
      }
    }
    if (securityValue) {
      std::wstring sddl = fromUtf8(securityValue->get().string);
      PSECURITY_DESCRIPTOR raw = nullptr;
      if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
          sddl.c_str(), SDDL_REVISION_1, &raw, nullptr)) {
        failWin("ConvertStringSecurityDescriptorToSecurityDescriptorW(service)");
      }
      Local<SECURITY_DESCRIPTOR> descriptor(static_cast<SECURITY_DESCRIPTOR*>(raw));
      SECURITY_DESCRIPTOR_CONTROL control = 0;
      DWORD revision = 0;
      if (!GetSecurityDescriptorControl(descriptor.get(), &control, &revision)) {
        failWin("GetSecurityDescriptorControl(service)");
      }
      SECURITY_INFORMATION info = DACL_SECURITY_INFORMATION |
          ((control & SE_DACL_PROTECTED)
              ? PROTECTED_DACL_SECURITY_INFORMATION
              : UNPROTECTED_DACL_SECURITY_INFORMATION);
      if (!SetServiceObjectSecurity(service.get(), info, descriptor.get())) {
        failWin("SetServiceObjectSecurity");
      }
    }
  }
  if (environmentValue) writeServiceEnvironment(args[0], environmentValue->get());
  emit("{\"configured\":true}");
}

SERVICE_STATUS_PROCESS queryStatus(SC_HANDLE service) {
  SERVICE_STATUS_PROCESS status{};
  DWORD bytes = 0;
  if (!QueryServiceStatusEx(service, SC_STATUS_PROCESS_INFO,
      reinterpret_cast<BYTE*>(&status), sizeof(status), &bytes)) failWin("QueryServiceStatusEx");
  return status;
}

void startServiceCommand(const std::vector<std::wstring>& args) {
  expect(args, 1, "service-start <service>");
  requireService(args[0]);
  ServiceHandle manager = scm();
  ServiceHandle service(OpenServiceW(manager.get(), args[0].c_str(), SERVICE_START | SERVICE_QUERY_STATUS));
  if (!service) failWin("OpenServiceW");
  if (!StartServiceW(service.get(), 0, nullptr) && GetLastError() != ERROR_SERVICE_ALREADY_RUNNING) {
    failWin("StartServiceW");
  }
  auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(60);
  SERVICE_STATUS_PROCESS status{};
  for (;;) {
    status = queryStatus(service.get());
    if (status.dwCurrentState == SERVICE_RUNNING) break;
    if (status.dwCurrentState == SERVICE_STOPPED) throw Error("service stopped while starting");
    if (std::chrono::steady_clock::now() >= deadline) throw Error("service start timed out");
    Sleep(std::clamp<DWORD>(status.dwWaitHint / 10, 50, 1000));
  }
  emit("{\"name\":" + json(args[0]) + ",\"state\":\"running\",\"pid\":" + std::to_string(status.dwProcessId) + "}");
}

void stopServiceCommand(const std::vector<std::wstring>& args) {
  if (args.size() != 3 || args[1] != L"--timeout-ms") {
    throw Error("usage: service-stop <service> --timeout-ms <milliseconds>");
  }
  requireService(args[0]);
  DWORD timeout = uint32Arg(args[2], "timeout");
  ServiceHandle manager = scm();
  ServiceHandle service(OpenServiceW(manager.get(), args[0].c_str(), SERVICE_STOP | SERVICE_QUERY_STATUS));
  if (!service) failWin("OpenServiceW");
  SERVICE_STATUS_PROCESS status = queryStatus(service.get());
  if (status.dwCurrentState != SERVICE_STOPPED) {
    SERVICE_STATUS control{};
    if (!ControlService(service.get(), SERVICE_CONTROL_STOP, &control)) {
      DWORD code = GetLastError();
      if (code != ERROR_SERVICE_NOT_ACTIVE) failWin("ControlService(STOP)", code);
    }
    auto deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds(timeout);
    for (;;) {
      status = queryStatus(service.get());
      if (status.dwCurrentState == SERVICE_STOPPED) break;
      if (std::chrono::steady_clock::now() >= deadline) throw Error("service stop timed out");
      Sleep(std::clamp<DWORD>(status.dwWaitHint / 10, 50, 1000));
    }
  }
  emit("{\"name\":" + json(args[0]) + ",\"state\":\"stopped\",\"pid\":0}");
}

DWORD servicePidIfRunning(SC_HANDLE manager, const wchar_t* name) {
  SC_HANDLE raw = OpenServiceW(manager, name, SERVICE_QUERY_STATUS);
  if (!raw) {
    DWORD code = GetLastError();
    if (code == ERROR_SERVICE_DOES_NOT_EXIST) return 0;
    failWin("OpenServiceW", code);
  }
  ServiceHandle service(raw);
  SERVICE_STATUS_PROCESS status = queryStatus(service.get());
  return status.dwCurrentState == SERVICE_STOPPED ? 0 : status.dwProcessId;
}

std::unordered_map<DWORD, DWORD> parentMap(DWORD* helperParent = nullptr) {
  Handle snapshot(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0));
  if (!snapshot) failWin("CreateToolhelp32Snapshot");
  PROCESSENTRY32W entry{};
  entry.dwSize = sizeof(entry);
  std::unordered_map<DWORD, DWORD> parents;
  if (Process32FirstW(snapshot.get(), &entry)) {
    do {
      parents.emplace(entry.th32ProcessID, entry.th32ParentProcessID);
      if (helperParent && entry.th32ProcessID == GetCurrentProcessId()) *helperParent = entry.th32ParentProcessID;
    } while (Process32NextW(snapshot.get(), &entry));
    if (GetLastError() != ERROR_NO_MORE_FILES) failWin("Process32NextW");
  }
  return parents;
}

void assertServiceContext(const std::vector<std::wstring>& args) {
  expect(args, 2, "assert-service-context RoostUpdaterV2 <pid>");
  if (args[0] != L"RoostUpdaterV2") throw Error("only RoostUpdaterV2 context can be asserted");
  DWORD pid = uint32Arg(args[1], "PID");
  ServiceHandle manager = scm();
  ServiceHandle updater(OpenServiceW(manager.get(), L"RoostUpdaterV2",
      SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS));
  if (!updater) failWin("OpenServiceW(RoostUpdaterV2)");
  ServiceConfigSnapshot config = queryConfig(updater.get());
  std::vector<std::wstring> configuredArgv = splitCommandLine(config.image);
  if (configuredArgv.empty() || lower(baseName(configuredArgv[0])) != L"shawl.exe") {
    throw Error("RoostUpdaterV2 is not SCM-configured through Shawl");
  }
  SERVICE_STATUS_PROCESS updaterStatus = queryStatus(updater.get());
  if (!updaterStatus.dwProcessId || lower(baseName(processImage(updaterStatus.dwProcessId))) != L"shawl.exe") {
    throw Error("RoostUpdaterV2 SCM process is not the configured Shawl host");
  }
  DWORD helperParent = 0;
  auto parents = parentMap(&helperParent);
  if (helperParent != pid) throw Error("asserted updater PID is not the helper parent");
  DWORD worker = servicePidIfRunning(manager.get(), L"RoostWorkerV2");
  DWORD coordinator = servicePidIfRunning(manager.get(), L"RoostCoordinatorV2");
  bool reachedUpdater = false;
  std::set<DWORD> visited;
  for (DWORD cursor = pid; cursor && visited.insert(cursor).second;) {
    if (cursor == worker || cursor == coordinator) {
      throw Error("updater is descended from a worker/coordinator service job");
    }
    if (cursor == updaterStatus.dwProcessId) { reachedUpdater = true; break; }
    auto found = parents.find(cursor);
    if (found == parents.end()) break;
    cursor = found->second;
  }
  if (!reachedUpdater) throw Error("updater process is not descended from RoostUpdaterV2 Shawl");
  emit("{\"service\":\"RoostUpdaterV2\",\"pid\":" + std::to_string(pid) +
      ",\"isolatedFromWorkerCoordinatorJobs\":true}");
}

std::vector<std::uint8_t> readFrame(HANDLE pipe, std::uint32_t maximum) {
  std::array<std::uint8_t, 4> prefix{};
  std::size_t offset = 0;
  while (offset < prefix.size()) {
    DWORD got = 0;
    if (!ReadFile(pipe, prefix.data() + offset, static_cast<DWORD>(prefix.size() - offset), &got, nullptr)) {
      failWin("ReadFile(control pipe)");
    }
    if (!got) throw Error("control pipe closed mid-frame");
    offset += got;
  }
  std::uint32_t length = prefix[0] | (std::uint32_t(prefix[1]) << 8) |
      (std::uint32_t(prefix[2]) << 16) | (std::uint32_t(prefix[3]) << 24);
  if (!length || length > maximum) throw Error("invalid control frame size");
  std::vector<std::uint8_t> frame(length);
  offset = 0;
  while (offset < frame.size()) {
    DWORD got = 0;
    if (!ReadFile(pipe, frame.data() + offset, static_cast<DWORD>(frame.size() - offset), &got, nullptr)) {
      failWin("ReadFile(control pipe)");
    }
    if (!got) throw Error("control pipe closed mid-frame");
    offset += got;
  }
  return frame;
}

void sendFrame(HANDLE pipe, const std::vector<std::uint8_t>& payload) {
  std::array<std::uint8_t, 4> prefix{
      static_cast<std::uint8_t>(payload.size()),
      static_cast<std::uint8_t>(payload.size() >> 8),
      static_cast<std::uint8_t>(payload.size() >> 16),
      static_cast<std::uint8_t>(payload.size() >> 24)};
  writeAll(pipe, prefix.data(), prefix.size());
  writeAll(pipe, payload.data(), payload.size());
}

class FrameCursor final {
 public:
  explicit FrameCursor(const std::vector<std::uint8_t>& value) : value_(value) {}
  void magic(const char* text) {
    if (position_ + 4 > value_.size() || std::memcmp(value_.data() + position_, text, 4)) {
      throw Error("invalid job-host request magic");
    }
    position_ += 4;
  }
  std::uint32_t u32() {
    if (position_ + 4 > value_.size()) throw Error("truncated job-host request");
    std::uint32_t value = value_[position_] | (std::uint32_t(value_[position_ + 1]) << 8) |
        (std::uint32_t(value_[position_ + 2]) << 16) | (std::uint32_t(value_[position_ + 3]) << 24);
    position_ += 4;
    return value;
  }
  std::wstring string() {
    std::uint32_t length = u32();
    if (length > 1024 * 1024 || position_ + length > value_.size()) throw Error("invalid job-host string length");
    std::string_view bytes(reinterpret_cast<const char*>(value_.data() + position_), length);
    if (bytes.find('\0') != std::string_view::npos) throw Error("NUL in job-host string");
    position_ += length;
    return fromUtf8(bytes);
  }
  void finish() const { if (position_ != value_.size()) throw Error("trailing job-host request bytes"); }
 private:
  const std::vector<std::uint8_t>& value_;
  std::size_t position_ = 0;
};

bool constantTimeEqual(const std::wstring& left, const std::wstring& right) {
  std::size_t maximum = std::max(left.size(), right.size());
  std::uint32_t difference = static_cast<std::uint32_t>(left.size() ^ right.size());
  for (std::size_t i = 0; i < maximum; ++i) {
    wchar_t a = i < left.size() ? left[i] : 0;
    wchar_t b = i < right.size() ? right[i] : 0;
    difference |= static_cast<std::uint32_t>(a ^ b);
  }
  return difference == 0;
}

void verifyPipeClient(HANDLE pipe) {
  ULONG pid = 0;
  if (!GetNamedPipeClientProcessId(pipe, &pid) || !pid) failWin("GetNamedPipeClientProcessId");
  Handle process(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid));
  if (!process) failWin("OpenProcess(pipe client)");
  HANDLE rawToken = nullptr;
  if (!OpenProcessToken(process.get(), TOKEN_QUERY, &rawToken)) failWin("OpenProcessToken(pipe client)");
  Handle token(rawToken);
  DWORD bytes = 0;
  GetTokenInformation(token.get(), TokenUser, nullptr, 0, &bytes);
  if (GetLastError() != ERROR_INSUFFICIENT_BUFFER) failWin("GetTokenInformation(pipe client)");
  std::vector<std::uint8_t> buffer(bytes);
  if (!GetTokenInformation(token.get(), TokenUser, buffer.data(), bytes, &bytes)) {
    failWin("GetTokenInformation(pipe client)");
  }
  auto mine = currentSid();
  if (!EqualSid(mine.data(), reinterpret_cast<TOKEN_USER*>(buffer.data())->User.Sid)) {
    throw Error("job-host pipe client SID does not match helper SID");
  }
}

std::vector<wchar_t> environmentBlock(const std::vector<std::pair<std::wstring, std::wstring>>& entries) {
  std::set<std::wstring> keys;
  std::vector<std::pair<std::wstring, std::wstring>> sorted = entries;
  for (const auto& [key, value] : sorted) {
    if (key.empty() || key.find(L'=') != std::wstring::npos || key.find(L'\0') != std::wstring::npos ||
        value.find(L'\0') != std::wstring::npos || !keys.insert(lower(key)).second) {
      throw Error("invalid or duplicate job-host environment key");
    }
  }
  std::sort(sorted.begin(), sorted.end(), [](const auto& a, const auto& b) { return lower(a.first) < lower(b.first); });
  std::vector<wchar_t> block;
  for (const auto& [key, value] : sorted) {
    block.insert(block.end(), key.begin(), key.end());
    block.push_back(L'=');
    block.insert(block.end(), value.begin(), value.end());
    block.push_back(L'\0');
  }
  block.push_back(L'\0');
  if (entries.empty()) block.push_back(L'\0');
  return block;
}

void jobHost(const std::vector<std::wstring>& args) {
  if (args.size() != 4 || args[0] != L"--pipe" || args[2] != L"--cap") {
    throw Error("usage: job-host --pipe <named-pipe> --cap <capability>");
  }
  if (args[1].rfind(L"\\\\.\\pipe\\roost-job-", 0) != 0 || args[3].size() != 64) {
    throw Error("invalid job-host pipe or capability");
  }
  for (wchar_t ch : args[3]) {
    if (!((ch >= L'0' && ch <= L'9') || (ch >= L'a' && ch <= L'f') || (ch >= L'A' && ch <= L'F'))) {
      throw Error("invalid job-host capability");
    }
  }
  auto sid = currentSid();
  std::wstring sddl = L"D:P(A;;GA;;;SY)(A;;GA;;;" + sidText(sid.data()) + L")";
  PSECURITY_DESCRIPTOR rawDescriptor = nullptr;
  if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.c_str(), SDDL_REVISION_1,
      &rawDescriptor, nullptr)) failWin("ConvertStringSecurityDescriptorToSecurityDescriptorW(pipe)");
  Local<SECURITY_DESCRIPTOR> descriptor(static_cast<SECURITY_DESCRIPTOR*>(rawDescriptor));
  SECURITY_ATTRIBUTES security{};
  security.nLength = sizeof(security);
  security.lpSecurityDescriptor = descriptor.get();
  Handle pipe(CreateNamedPipeW(args[1].c_str(),
      PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
      PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
      1, 64 * 1024, 64 * 1024, 0, &security));
  if (!pipe) failWin("CreateNamedPipeW");
  if (!ConnectNamedPipe(pipe.get(), nullptr) && GetLastError() != ERROR_PIPE_CONNECTED) {
    failWin("ConnectNamedPipe");
  }
  verifyPipeClient(pipe.get());
  std::vector<std::uint8_t> request = readFrame(pipe.get(), kMaxFrame);
  FrameCursor cursor(request);
  cursor.magic("RJH1");
  std::wstring capability = cursor.string();
  if (!constantTimeEqual(lower(capability), lower(args[3]))) throw Error("job-host capability rejected");
  std::wstring executable = cursor.string();
  std::wstring cwd = cursor.string();
  if (executable.empty() || cwd.empty()) throw Error("job-host executable and cwd are required");
  std::uint32_t argc = cursor.u32();
  if (argc > 4096) throw Error("too many job-host arguments");
  std::vector<std::wstring> childArgs;
  childArgs.reserve(argc);
  for (std::uint32_t i = 0; i < argc; ++i) childArgs.push_back(cursor.string());
  std::uint32_t envc = cursor.u32();
  if (envc > 65536) throw Error("too many job-host environment entries");
  std::vector<std::pair<std::wstring, std::wstring>> environment;
  environment.reserve(envc);
  for (std::uint32_t i = 0; i < envc; ++i) environment.emplace_back(cursor.string(), cursor.string());
  cursor.finish();
  std::vector<wchar_t> env = environmentBlock(environment);
  std::wstring command = quoteArg(executable);
  for (const std::wstring& argument : childArgs) command += L" " + quoteArg(argument);
  std::vector<wchar_t> mutableCommand(command.begin(), command.end());
  mutableCommand.push_back(L'\0');

  Handle completion(CreateIoCompletionPort(INVALID_HANDLE_VALUE, nullptr, 0, 1));
  if (!completion) failWin("CreateIoCompletionPort");
  Handle job(CreateJobObjectW(nullptr, nullptr));
  if (!job) failWin("CreateJobObjectW");
  JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
  limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
  if (!SetInformationJobObject(job.get(), JobObjectExtendedLimitInformation, &limits, sizeof(limits))) {
    failWin("SetInformationJobObject");
  }
  JOBOBJECT_ASSOCIATE_COMPLETION_PORT association{};
  association.CompletionKey = job.get();
  association.CompletionPort = completion.get();
  if (!SetInformationJobObject(job.get(), JobObjectAssociateCompletionPortInformation,
      &association, sizeof(association))) failWin("SetInformationJobObject(completion)");
  STARTUPINFOW startup{};
  startup.cb = sizeof(startup);
  startup.dwFlags = STARTF_USESTDHANDLES;
  startup.hStdInput = GetStdHandle(STD_INPUT_HANDLE);
  startup.hStdOutput = GetStdHandle(STD_OUTPUT_HANDLE);
  startup.hStdError = GetStdHandle(STD_ERROR_HANDLE);
  PROCESS_INFORMATION process{};
  if (!CreateProcessW(executable.c_str(), mutableCommand.data(), nullptr, nullptr, TRUE,
      CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT, env.data(), cwd.c_str(), &startup, &process)) {
    failWin("CreateProcessW(job child)");
  }
  Handle child(process.hProcess);
  Handle childThread(process.hThread);
  if (!AssignProcessToJobObject(job.get(), child.get())) {
    TerminateProcess(child.get(), ERROR_ACCESS_DENIED);
    failWin("AssignProcessToJobObject");
  }
  if (ResumeThread(childThread.get()) == static_cast<DWORD>(-1)) failWin("ResumeThread");
  std::vector<std::uint8_t> assigned{1,
      static_cast<std::uint8_t>(process.dwProcessId),
      static_cast<std::uint8_t>(process.dwProcessId >> 8),
      static_cast<std::uint8_t>(process.dwProcessId >> 16),
      static_cast<std::uint8_t>(process.dwProcessId >> 24)};
  sendFrame(pipe.get(), assigned);

  bool explicitClose = false;
  bool disconnected = false;
  bool killed = false;
  bool empty = false;
  auto killDeadline = std::chrono::steady_clock::time_point::max();
  while (!empty) {
    DWORD message = 0;
    ULONG_PTR key = 0;
    LPOVERLAPPED overlapped = nullptr;
    if (GetQueuedCompletionStatus(completion.get(), &message, &key, &overlapped, 20)) {
      if (key == reinterpret_cast<ULONG_PTR>(association.CompletionKey) &&
          message == JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO) empty = true;
    } else {
      DWORD code = GetLastError();
      if (code != WAIT_TIMEOUT) failWin("GetQueuedCompletionStatus", code);
    }
    if (!killed && !empty) {
      DWORD available = 0;
      if (!PeekNamedPipe(pipe.get(), nullptr, 0, nullptr, &available, nullptr)) {
        DWORD code = GetLastError();
        if (code == ERROR_BROKEN_PIPE || code == ERROR_PIPE_NOT_CONNECTED) disconnected = true;
        else failWin("PeekNamedPipe", code);
      } else if (available >= 4) {
        std::vector<std::uint8_t> close = readFrame(pipe.get(), 64);
        if (close.size() != 1 || close[0] != 4) throw Error("invalid job-host close frame");
        explicitClose = true;
      }
      if (explicitClose || disconnected) {
        job.reset();
        killed = true;
        killDeadline = std::chrono::steady_clock::now() + std::chrono::seconds(30);
      }
    }
    if (killed && !empty && std::chrono::steady_clock::now() >= killDeadline) {
      throw Error("job-host timed out waiting for ACTIVE_PROCESS_ZERO");
    }
  }
  DWORD childExit = 0;
  std::int32_t reported = -1;
  if (!killed && WaitForSingleObject(child.get(), 0) == WAIT_OBJECT_0 &&
      GetExitCodeProcess(child.get(), &childExit) && childExit <= INT32_MAX) {
    reported = static_cast<std::int32_t>(childExit);
  }
  if (job) job.reset();
  if (!disconnected) {
    std::vector<std::uint8_t> closed{2,
        static_cast<std::uint8_t>(reported),
        static_cast<std::uint8_t>(reported >> 8),
        static_cast<std::uint8_t>(reported >> 16),
        static_cast<std::uint8_t>(reported >> 24)};
    sendFrame(pipe.get(), closed);
    FlushFileBuffers(pipe.get());
  }
}

}  // namespace roost

int wmain(int argc, wchar_t** argv) {
  SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
  try {
    if (argc < 2) throw roost::Error("usage: roost-win-helper <operation> [arguments]");
    const std::wstring operation = argv[1];
    std::vector<std::wstring> arguments;
    arguments.reserve(static_cast<std::size_t>(argc - 2));
    for (int index = 2; index < argc; ++index) arguments.emplace_back(argv[index]);

    if (operation == L"version" || operation == L"--version") {
      roost::expect(arguments, 0, "version");
      roost::emit(
          "{\"protocol\":1,\"helper\":\"roost-win-helper\",\"arch\":\"x64\",\"commands\":["
          "\"version\",\"flush-file\",\"replace-file\",\"remove-file\",\"apply-dacl\","
          "\"get-dacl\",\"apply-sddl\",\"probe-exclusive-open\",\"current-user-sid\","
          "\"host-sample\",\"process-snapshot\",\"listening-ports\",\"verify-cms-detached\","
          "\"verify-authenticode\",\"extract-zip\",\"assert-service-context\","
          "\"resolve-account-sid\",\"grant-logon-as-service\",\"apply-service-dacl\","
          "\"configure-service-account\",\"service-query\",\"service-config\","
          "\"service-start\",\"service-stop\",\"job-host\"]}");
    } else if (operation == L"flush-file") {
      roost::flushFile(arguments);
    } else if (operation == L"replace-file") {
      roost::replaceFile(arguments);
    } else if (operation == L"remove-file") {
      roost::removeFile(arguments);
    } else if (operation == L"apply-dacl") {
      roost::applyDacl(arguments);
    } else if (operation == L"get-dacl") {
      roost::getDacl(arguments);
    } else if (operation == L"apply-sddl") {
      roost::applySddl(arguments);
    } else if (operation == L"probe-exclusive-open") {
      roost::exclusiveOpen(arguments);
    } else if (operation == L"current-user-sid") {
      roost::currentUserSid(arguments);
    } else if (operation == L"host-sample") {
      roost::hostSample(arguments);
    } else if (operation == L"process-snapshot") {
      roost::processSnapshot(arguments);
    } else if (operation == L"listening-ports") {
      roost::listeningPorts(arguments);
    } else if (operation == L"verify-cms-detached") {
      roost::verifyDetachedCms(arguments);
    } else if (operation == L"verify-authenticode") {
      roost::verifyAuthenticode(arguments);
    } else if (operation == L"extract-zip") {
      roost::extractZip(arguments);
    } else if (operation == L"resolve-account-sid") {
      roost::resolveAccountSid(arguments);
    } else if (operation == L"grant-logon-as-service") {
      roost::grantLogonAsService(arguments);
    } else if (operation == L"apply-service-dacl") {
      roost::applyServiceDacl(arguments);
    } else if (operation == L"configure-service-account") {
      roost::configureServiceAccount(arguments);
    } else if (operation == L"service-query") {
      roost::queryServiceCommand(arguments);
    } else if (operation == L"service-config") {
      roost::configureServiceCommand(arguments);
    } else if (operation == L"service-start") {
      roost::startServiceCommand(arguments);
    } else if (operation == L"service-stop") {
      roost::stopServiceCommand(arguments);
    } else if (operation == L"assert-service-context") {
      roost::assertServiceContext(arguments);
    } else if (operation == L"job-host") {
      roost::jobHost(arguments);
    } else {
      throw roost::Error("unknown helper operation");
    }
    return 0;
  } catch (const std::exception& error) {
    try {
      roost::emit("{\"error\":" + roost::json(error.what()) + "}");
    } catch (...) {
      // The only remaining output path is intentionally silent.
    }
    return 1;
  }
}
