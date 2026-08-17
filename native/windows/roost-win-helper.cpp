#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#include <aclapi.h>
#include <bcrypt.h>
#include <iphlpapi.h>
#include <lm.h>
#include <ntsecapi.h>
#include <sddl.h>
#include <shellapi.h>
#include <shlobj.h>
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

std::vector<std::uint8_t> lookupAccountSid(const std::wstring& account) {
  if (account.empty()) throw Error("account name must not be empty");
  DWORD sidBytes = 0;
  DWORD domainChars = 0;
  SID_NAME_USE use{};
  LookupAccountNameW(nullptr, account.c_str(), nullptr, &sidBytes, nullptr, &domainChars, &use);
  if (GetLastError() != ERROR_INSUFFICIENT_BUFFER || !sidBytes) failWin("LookupAccountNameW");
  std::vector<std::uint8_t> sid(sidBytes);
  std::wstring domain(domainChars, L'\0');
  if (!LookupAccountNameW(nullptr, account.c_str(), sid.data(), &sidBytes,
      domain.data(), &domainChars, &use)) failWin("LookupAccountNameW");
  sid.resize(sidBytes);
  if (!IsValidSid(sid.data())) throw Error("account resolved to an invalid SID");
  return sid;
}

bool equalOrdinalIgnoreCase(const std::wstring& left, const std::wstring& right) {
  return CompareStringOrdinal(left.data(), static_cast<int>(left.size()),
      right.data(), static_cast<int>(right.size()), TRUE) == CSTR_EQUAL;
}

std::wstring localComputerName() {
  std::array<wchar_t, MAX_COMPUTERNAME_LENGTH + 1> buffer{};
  DWORD length = static_cast<DWORD>(buffer.size());
  if (!GetComputerNameW(buffer.data(), &length)) failWin("GetComputerNameW");
  return std::wstring(buffer.data(), length);
}

std::wstring accountNameForSid(PSID sid) {
  DWORD accountChars = 0;
  DWORD domainChars = 0;
  SID_NAME_USE use{};
  LookupAccountSidW(nullptr, sid, nullptr, &accountChars, nullptr, &domainChars, &use);
  if (GetLastError() != ERROR_INSUFFICIENT_BUFFER) failWin("LookupAccountSidW");
  std::wstring account(accountChars, L'\0');
  std::wstring domain(domainChars, L'\0');
  if (!LookupAccountSidW(nullptr, sid, account.data(), &accountChars,
      domain.data(), &domainChars, &use)) failWin("LookupAccountSidW");
  account.resize(accountChars);
  return account;
}
bool userIsAdministrator(const std::wstring& account) {
  std::array<std::uint8_t, SECURITY_MAX_SID_SIZE> administratorSid{};
  DWORD administratorSidBytes = static_cast<DWORD>(administratorSid.size());
  if (!CreateWellKnownSid(WinBuiltinAdministratorsSid, nullptr,
      administratorSid.data(), &administratorSidBytes)) failWin("CreateWellKnownSid");
  const std::wstring administratorName = accountNameForSid(administratorSid.data());

  LPBYTE rawGroups = nullptr;
  DWORD entriesRead = 0;
  DWORD totalEntries = 0;
  const NET_API_STATUS status = NetUserGetLocalGroups(
      nullptr, account.c_str(), 0, LG_INCLUDE_INDIRECT, &rawGroups,
      MAX_PREFERRED_LENGTH, &entriesRead, &totalEntries);
  if (status != NERR_Success) {
    if (rawGroups) NetApiBufferFree(rawGroups);
    throw Error("NetUserGetLocalGroups failed [netapi=" + std::to_string(status) + "]");
  }
  bool administrator = false;
  try {
    const auto* groups = reinterpret_cast<const LOCALGROUP_USERS_INFO_0*>(rawGroups);
    for (DWORD index = 0; index < entriesRead; ++index) {
      if (groups[index].lgrui0_name &&
          equalOrdinalIgnoreCase(groups[index].lgrui0_name, administratorName)) {
        administrator = true;
        break;
      }
    }
  } catch (...) {
    if (rawGroups) NetApiBufferFree(rawGroups);
    throw;
  }
  if (rawGroups) NetApiBufferFree(rawGroups);
  return administrator;
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

constexpr wchar_t kKeeperServiceName[] = L"RoostKeeperV2";
constexpr wchar_t kWorkerServiceName[] = L"RoostWorkerV2";
constexpr wchar_t kCoordinatorServiceName[] = L"RoostCoordinatorV2";
constexpr wchar_t kUpdaterServiceName[] = L"RoostUpdaterV2";
constexpr wchar_t kKeeperServiceAccount[] = L"NT SERVICE\\RoostKeeperV2";
constexpr wchar_t kWorkerServiceAccount[] = L"NT SERVICE\\RoostWorkerV2";
constexpr wchar_t kCoordinatorServiceAccount[] = L"NT SERVICE\\RoostCoordinatorV2";
constexpr wchar_t kUpdaterServiceAccount[] = L"NT SERVICE\\RoostUpdaterV2";
constexpr wchar_t kDirectoryModifyRights[] = L"0x1301bf";

class BCryptAlgorithm final {
 public:
  explicit BCryptAlgorithm(BCRYPT_ALG_HANDLE value) : value_(value) {}
  ~BCryptAlgorithm() {
    if (value_) BCryptCloseAlgorithmProvider(value_, 0);
  }
  BCRYPT_ALG_HANDLE get() const { return value_; }
 private:
  BCRYPT_ALG_HANDLE value_;
};

class BCryptHash final {
 public:
  explicit BCryptHash(BCRYPT_HASH_HANDLE value) : value_(value) {}
  ~BCryptHash() {
    if (value_) BCryptDestroyHash(value_);
  }
  BCRYPT_HASH_HANDLE get() const { return value_; }
 private:
  BCRYPT_HASH_HANDLE value_;
};

std::vector<std::uint8_t> serviceSidForName(const std::wstring& service) {
  if (service != kKeeperServiceName &&
      service != kWorkerServiceName &&
      service != kCoordinatorServiceName &&
      service != kUpdaterServiceName) {
    throw Error("service SID name is not allowlisted");
  }
  int uppercaseChars = LCMapStringEx(
      LOCALE_NAME_INVARIANT,
      LCMAP_UPPERCASE,
      service.data(),
      static_cast<int>(service.size()),
      nullptr,
      0,
      nullptr,
      nullptr,
      0);
  if (!uppercaseChars) failWin("LCMapStringEx(service SID)");
  std::wstring uppercase(static_cast<std::size_t>(uppercaseChars), L'\0');
  if (!LCMapStringEx(
      LOCALE_NAME_INVARIANT,
      LCMAP_UPPERCASE,
      service.data(),
      static_cast<int>(service.size()),
      uppercase.data(),
      uppercaseChars,
      nullptr,
      nullptr,
      0)) {
    failWin("LCMapStringEx(service SID)");
  }

  BCRYPT_ALG_HANDLE rawAlgorithm = nullptr;
  if (BCryptOpenAlgorithmProvider(
      &rawAlgorithm, BCRYPT_SHA1_ALGORITHM, nullptr, 0) < 0) {
    throw Error("BCryptOpenAlgorithmProvider(service SID) failed");
  }
  BCryptAlgorithm algorithm(rawAlgorithm);
  DWORD objectBytes = 0;
  DWORD returned = 0;
  if (BCryptGetProperty(
      algorithm.get(),
      BCRYPT_OBJECT_LENGTH,
      reinterpret_cast<PUCHAR>(&objectBytes),
      sizeof(objectBytes),
      &returned,
      0) < 0) {
    throw Error("BCryptGetProperty(service SID) failed");
  }
  std::vector<std::uint8_t> object(objectBytes);
  BCRYPT_HASH_HANDLE rawHash = nullptr;
  if (BCryptCreateHash(
      algorithm.get(),
      &rawHash,
      object.data(),
      static_cast<ULONG>(object.size()),
      nullptr,
      0,
      0) < 0) {
    throw Error("BCryptCreateHash(service SID) failed");
  }
  BCryptHash hash(rawHash);
  const std::size_t nameBytes = uppercase.size() * sizeof(wchar_t);
  if (nameBytes > ULONG_MAX ||
      BCryptHashData(
          hash.get(),
          reinterpret_cast<PUCHAR>(uppercase.data()),
          static_cast<ULONG>(nameBytes),
          0) < 0) {
    throw Error("BCryptHashData(service SID) failed");
  }
  std::array<std::uint8_t, 20> digest{};
  if (BCryptFinishHash(
      hash.get(), digest.data(), static_cast<ULONG>(digest.size()), 0) < 0) {
    throw Error("BCryptFinishHash(service SID) failed");
  }

  SID_IDENTIFIER_AUTHORITY authority = SECURITY_NT_AUTHORITY;
  std::vector<std::uint8_t> sid(GetSidLengthRequired(6));
  if (!InitializeSid(sid.data(), &authority, 6)) {
    failWin("InitializeSid(service SID)");
  }
  *GetSidSubAuthority(sid.data(), 0) = SECURITY_SERVICE_ID_BASE_RID;
  for (DWORD index = 0; index < 5; ++index) {
    const std::size_t offset = static_cast<std::size_t>(index) * 4;
    *GetSidSubAuthority(sid.data(), index + 1) =
        static_cast<DWORD>(digest[offset]) |
        (static_cast<DWORD>(digest[offset + 1]) << 8) |
        (static_cast<DWORD>(digest[offset + 2]) << 16) |
        (static_cast<DWORD>(digest[offset + 3]) << 24);
  }
  if (!IsValidSid(sid.data())) throw Error("derived service SID is invalid");
  return sid;
}

std::vector<std::uint8_t> serviceSidForAccount(const std::wstring& account) {
  if (equalOrdinalIgnoreCase(account, kKeeperServiceAccount)) {
    return serviceSidForName(kKeeperServiceName);
  }
  if (equalOrdinalIgnoreCase(account, kWorkerServiceAccount)) {
    return serviceSidForName(kWorkerServiceName);
  }
  if (equalOrdinalIgnoreCase(account, kCoordinatorServiceAccount)) {
    return serviceSidForName(kCoordinatorServiceName);
  }
  if (equalOrdinalIgnoreCase(account, kUpdaterServiceAccount)) {
    return serviceSidForName(kUpdaterServiceName);
  }
  throw Error("service virtual account is not allowlisted");
}

std::vector<std::uint8_t> sidFromText(const std::wstring& text, const char* label) {
  if (text.empty()) throw Error(std::string(label) + " must not be empty");
  PSID raw = nullptr;
  if (!ConvertStringSidToSidW(text.c_str(), &raw)) failWin("ConvertStringSidToSidW");
  Local<void> sid(raw);
  if (!IsValidSid(sid.get())) throw Error(std::string("invalid ") + label);
  std::vector<std::uint8_t> out(GetLengthSid(sid.get()));
  if (!CopySid(static_cast<DWORD>(out.size()), out.data(), sid.get())) failWin("CopySid");
  return out;
}

std::optional<std::wstring> processEnvironmentValue(const wchar_t* name) {
  SetLastError(ERROR_SUCCESS);
  DWORD needed = GetEnvironmentVariableW(name, nullptr, 0);
  if (!needed) {
    const DWORD code = GetLastError();
    if (code == ERROR_ENVVAR_NOT_FOUND) return std::nullopt;
    if (code == ERROR_SUCCESS) return std::wstring();
    failWin("GetEnvironmentVariableW", code);
  }
  std::vector<wchar_t> buffer(needed);
  const DWORD written = GetEnvironmentVariableW(
      name, buffer.data(), static_cast<DWORD>(buffer.size()));
  if (written + 1 != needed) failWin("GetEnvironmentVariableW");
  return std::wstring(buffer.data(), written);
}

bool exactAcl(PACL expected, PACL actual) {
  if (!expected || !actual || !IsValidAcl(expected) || !IsValidAcl(actual) ||
      expected->AclRevision != actual->AclRevision ||
      expected->AceCount != actual->AceCount) {
    return false;
  }
  for (DWORD index = 0; index < expected->AceCount; ++index) {
    void* expectedAce = nullptr;
    void* actualAce = nullptr;
    if (!GetAce(expected, index, &expectedAce) || !GetAce(actual, index, &actualAce)) {
      failWin("GetAce");
    }
    const auto* expectedHeader = static_cast<const ACE_HEADER*>(expectedAce);
    const auto* actualHeader = static_cast<const ACE_HEADER*>(actualAce);
    if (expectedHeader->AceSize != actualHeader->AceSize ||
        std::memcmp(expectedAce, actualAce, expectedHeader->AceSize) != 0) {
      return false;
    }
  }
  return true;
}

void setAndVerifyFileSecurity(HANDLE object, const std::wstring& sddl) {
  PSECURITY_DESCRIPTOR rawExpected = nullptr;
  if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
      sddl.c_str(), SDDL_REVISION_1, &rawExpected, nullptr)) {
    failWin("ConvertStringSecurityDescriptorToSecurityDescriptorW");
  }
  Local<SECURITY_DESCRIPTOR> expected(static_cast<SECURITY_DESCRIPTOR*>(rawExpected));
  PSID expectedOwner = nullptr;
  BOOL ownerDefaulted = FALSE;
  if (!GetSecurityDescriptorOwner(expected.get(), &expectedOwner, &ownerDefaulted) ||
      !expectedOwner || !IsValidSid(expectedOwner)) {
    throw Error("security template must contain a valid owner");
  }
  PACL expectedDacl = nullptr;
  BOOL daclPresent = FALSE;
  BOOL daclDefaulted = FALSE;
  if (!GetSecurityDescriptorDacl(
      expected.get(), &daclPresent, &expectedDacl, &daclDefaulted) ||
      !daclPresent || !expectedDacl || !IsValidAcl(expectedDacl)) {
    throw Error("security template must contain a valid DACL");
  }
  SECURITY_DESCRIPTOR_CONTROL expectedControl = 0;
  DWORD expectedRevision = 0;
  if (!GetSecurityDescriptorControl(
          expected.get(), &expectedControl, &expectedRevision)) {
    failWin("GetSecurityDescriptorControl(expected)");
  }

  const SECURITY_INFORMATION protection =
      (expectedControl & SE_DACL_PROTECTED)
      ? PROTECTED_DACL_SECURITY_INFORMATION
      : UNPROTECTED_DACL_SECURITY_INFORMATION;
  DWORD code = SetSecurityInfo(
      object,
      SE_FILE_OBJECT,
      OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | protection,
      expectedOwner,
      nullptr,
      expectedDacl,
      nullptr);
  if (code != ERROR_SUCCESS) failWin("SetSecurityInfo", code);

  PSECURITY_DESCRIPTOR rawActual = nullptr;
  PSID actualOwner = nullptr;
  PACL actualDacl = nullptr;
  code = GetSecurityInfo(
      object,
      SE_FILE_OBJECT,
      OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
      &actualOwner,
      nullptr,
      &actualDacl,
      nullptr,
      &rawActual);
  if (code != ERROR_SUCCESS) failWin("GetSecurityInfo", code);
  Local<SECURITY_DESCRIPTOR> actual(static_cast<SECURITY_DESCRIPTOR*>(rawActual));
  SECURITY_DESCRIPTOR_CONTROL control = 0;
  DWORD revision = 0;
  if (!GetSecurityDescriptorControl(actual.get(), &control, &revision)) {
    failWin("GetSecurityDescriptorControl");
  }
  BOOL actualPresent = FALSE;
  BOOL actualDefaulted = FALSE;
  PACL descriptorDacl = nullptr;
  if (!GetSecurityDescriptorDacl(
      actual.get(), &actualPresent, &descriptorDacl, &actualDefaulted)) {
    failWin("GetSecurityDescriptorDacl");
  }
  if (!actualOwner || !EqualSid(expectedOwner, actualOwner) ||
      ((control & SE_DACL_PROTECTED) !=
       (expectedControl & SE_DACL_PROTECTED)) ||
      !actualPresent || descriptorDacl != actualDacl ||
      !exactAcl(expectedDacl, actualDacl)) {
    throw Error("security owner or DACL verification failed");
  }
}

void appendDirectoryAllow(
    std::wstring& sddl,
    bool inherit,
    const wchar_t* rights,
    const std::wstring& sid) {
  sddl += inherit ? L"(A;OICI;" : L"(A;;";
  sddl += rights;
  sddl += L";;;";
  sddl += sid;
  sddl += L")";
}

std::wstring directoryProtectionSddl(
    const std::wstring& profile,
    const std::wstring& baseAccount,
    const std::wstring& interactiveSidText) {
  if (profile != L"install-root" &&
      profile != L"stable-bin-bootstrap" &&
      profile != L"stable-bin" &&
      profile != L"versions-bootstrap" &&
      profile != L"versions-root" &&
      profile != L"service-root" &&
      profile != L"service-home" &&
      profile != L"update-inbox" &&
      profile != L"local-update-inbox" &&
      profile != L"keeper-state" &&
      profile != L"worker-state" &&
      profile != L"coordinator-state" &&
      profile != L"updater-state") {
    throw Error("directory protection profile is not allowlisted");
  }

  const auto baseSid = lookupAccountSid(baseAccount);
  const auto interactiveSid = sidFromText(interactiveSidText, "interactive SID");
  const std::wstring base = sidText(const_cast<std::uint8_t*>(baseSid.data()));
  const std::wstring interactive =
      sidText(const_cast<std::uint8_t*>(interactiveSid.data()));
  std::map<std::wstring, std::wstring> resolvedServices;
  auto serviceSid = [&](const wchar_t* account) -> const std::wstring& {
    auto found = resolvedServices.find(account);
    if (found != resolvedServices.end()) return found->second;
    const auto sid = serviceSidForAccount(account);
    return resolvedServices.emplace(account, sidText(const_cast<std::uint8_t*>(sid.data())))
        .first->second;
  };

  std::wstring sddl = L"O:BAD:P";
  appendDirectoryAllow(sddl, true, L"FA", L"SY");
  appendDirectoryAllow(sddl, true, L"FA", L"BA");
  if (profile == L"install-root") {
    appendDirectoryAllow(sddl, false, L"GRGX", base);
    appendDirectoryAllow(sddl, false, L"GRGX", interactive);
  } else if (profile == L"stable-bin-bootstrap") {
    appendDirectoryAllow(sddl, true, L"GRGX", interactive);
  } else if (profile == L"stable-bin") {
    appendDirectoryAllow(sddl, true, L"GRGX", interactive);
    appendDirectoryAllow(sddl, true, L"GRGX", serviceSid(kKeeperServiceAccount));
    appendDirectoryAllow(sddl, true, L"GRGX", serviceSid(kWorkerServiceAccount));
    appendDirectoryAllow(sddl, true, L"GRGX", serviceSid(kCoordinatorServiceAccount));
    appendDirectoryAllow(
        sddl, true, kDirectoryModifyRights, serviceSid(kUpdaterServiceAccount));
  } else if (profile == L"versions-bootstrap") {
    appendDirectoryAllow(sddl, true, L"RC", L"S-1-3-4");
    appendDirectoryAllow(sddl, true, L"GRGX", base);
    appendDirectoryAllow(sddl, true, L"GRGX", interactive);
    appendDirectoryAllow(sddl, true, L"GRGX", serviceSid(kKeeperServiceAccount));
    appendDirectoryAllow(sddl, true, L"GRGX", serviceSid(kWorkerServiceAccount));
    appendDirectoryAllow(sddl, true, L"GRGX", serviceSid(kCoordinatorServiceAccount));
    appendDirectoryAllow(sddl, true, L"GRGX", serviceSid(kUpdaterServiceAccount));
  } else if (profile == L"versions-root") {
    appendDirectoryAllow(sddl, true, L"RC", L"S-1-3-4");
    appendDirectoryAllow(sddl, true, L"GRGX", base);
    appendDirectoryAllow(sddl, true, L"GRGX", interactive);
    appendDirectoryAllow(
        sddl, true, kDirectoryModifyRights, serviceSid(kUpdaterServiceAccount));
    appendDirectoryAllow(sddl, true, L"GRGX", serviceSid(kKeeperServiceAccount));
    appendDirectoryAllow(sddl, true, L"GRGX", serviceSid(kWorkerServiceAccount));
    appendDirectoryAllow(sddl, true, L"GRGX", serviceSid(kCoordinatorServiceAccount));
  } else if (profile == L"service-root") {
    appendDirectoryAllow(
        sddl, true, kDirectoryModifyRights, serviceSid(kUpdaterServiceAccount));
    appendDirectoryAllow(sddl, true, L"GRGX", serviceSid(kKeeperServiceAccount));
    appendDirectoryAllow(sddl, true, L"GRGX", serviceSid(kWorkerServiceAccount));
    appendDirectoryAllow(sddl, true, L"GRGX", serviceSid(kCoordinatorServiceAccount));
    appendDirectoryAllow(sddl, false, L"GRGX", interactive);
  } else if (profile == L"service-home") {
    appendDirectoryAllow(
        sddl, true, kDirectoryModifyRights, serviceSid(kKeeperServiceAccount));
    appendDirectoryAllow(
        sddl, true, kDirectoryModifyRights, serviceSid(kWorkerServiceAccount));
    appendDirectoryAllow(
        sddl, true, kDirectoryModifyRights, serviceSid(kCoordinatorServiceAccount));
    appendDirectoryAllow(
        sddl, true, kDirectoryModifyRights, serviceSid(kUpdaterServiceAccount));
    appendDirectoryAllow(sddl, true, L"GRGX", interactive);
  } else if (profile == L"update-inbox") {
    appendDirectoryAllow(sddl, true, L"RC", L"S-1-3-4");
    appendDirectoryAllow(
        sddl, true, kDirectoryModifyRights, serviceSid(kUpdaterServiceAccount));
    appendDirectoryAllow(
        sddl, false, L"0x00100022", serviceSid(kWorkerServiceAccount));
    appendDirectoryAllow(
        sddl, false, L"0x00100022",
        serviceSid(kCoordinatorServiceAccount));
    appendDirectoryAllow(sddl, false, L"0x00100020", interactive);
  } else if (profile == L"local-update-inbox") {
    appendDirectoryAllow(sddl, true, L"RC", L"S-1-3-4");
    appendDirectoryAllow(
        sddl, true, kDirectoryModifyRights, serviceSid(kUpdaterServiceAccount));
    appendDirectoryAllow(sddl, false, L"0x00100022", interactive);
  } else if (profile == L"keeper-state") {
    appendDirectoryAllow(sddl, true, L"RC", L"S-1-3-4");
    appendDirectoryAllow(
        sddl, true, kDirectoryModifyRights, serviceSid(kKeeperServiceAccount));
    appendDirectoryAllow(sddl, true, L"GRGX", serviceSid(kWorkerServiceAccount));
    appendDirectoryAllow(sddl, true, L"GRGX", serviceSid(kUpdaterServiceAccount));
    appendDirectoryAllow(sddl, true, L"GRGX", interactive);
  } else if (profile == L"worker-state") {
    appendDirectoryAllow(sddl, true, L"RC", L"S-1-3-4");
    appendDirectoryAllow(
        sddl, true, kDirectoryModifyRights, serviceSid(kWorkerServiceAccount));
    appendDirectoryAllow(sddl, false, L"0x001200a4", serviceSid(kUpdaterServiceAccount));
  } else if (profile == L"coordinator-state") {
    appendDirectoryAllow(sddl, true, L"RC", L"S-1-3-4");
    appendDirectoryAllow(
        sddl, true, kDirectoryModifyRights, serviceSid(kCoordinatorServiceAccount));
    appendDirectoryAllow(sddl, false, L"0x001200e2", serviceSid(kUpdaterServiceAccount));
    appendDirectoryAllow(sddl, true, L"GR", serviceSid(kUpdaterServiceAccount));
  } else if (profile == L"updater-state") {
    appendDirectoryAllow(sddl, true, L"RC", L"S-1-3-4");
    appendDirectoryAllow(
        sddl, true, kDirectoryModifyRights, serviceSid(kUpdaterServiceAccount));
    appendDirectoryAllow(sddl, false, L"GX", serviceSid(kWorkerServiceAccount));
    appendDirectoryAllow(sddl, false, L"GX", serviceSid(kCoordinatorServiceAccount));
    appendDirectoryAllow(sddl, false, L"0x00100020", interactive);
  } else {
    throw Error("directory protection profile is not implemented");
  }
  return sddl;
}

void protectDirectory(const std::vector<std::wstring>& args) {
  expect(args, 4, "protect-directory <path> <profile> <base-account> <interactive-sid>");
  const std::wstring& profile = args[1];
  const std::wstring sddl =
      directoryProtectionSddl(profile, args[2], args[3]);

  Handle directory(CreateFileW(
      args[0].c_str(),
      READ_CONTROL | WRITE_DAC | WRITE_OWNER,
      FILE_SHARE_READ | FILE_SHARE_WRITE,
      nullptr,
      OPEN_EXISTING,
      FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
      nullptr));
  if (!directory) failWin("CreateFileW(protected directory)");
  FILE_ATTRIBUTE_TAG_INFO attributes{};
  if (!GetFileInformationByHandleEx(
      directory.get(), FileAttributeTagInfo, &attributes, sizeof(attributes))) {
    failWin("GetFileInformationByHandleEx(protected directory)");
  }
  if (!(attributes.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) ||
      (attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT)) {
    throw Error("protected path is not a non-reparse directory");
  }
  setAndVerifyFileSecurity(directory.get(), sddl);
  emit("{\"protected\":true,\"profile\":" + json(profile) + "}");
}

bool setAndVerifyFileSecurityWithOptionalOwner(
    HANDLE object,
    const std::wstring& sddl) {
  PSECURITY_DESCRIPTOR rawExpected = nullptr;
  if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
      sddl.c_str(), SDDL_REVISION_1, &rawExpected, nullptr)) {
    failWin("ConvertStringSecurityDescriptorToSecurityDescriptorW");
  }
  Local<SECURITY_DESCRIPTOR> expected(static_cast<SECURITY_DESCRIPTOR*>(rawExpected));
  PSID expectedOwner = nullptr;
  BOOL ownerDefaulted = FALSE;
  PACL expectedDacl = nullptr;
  BOOL daclPresent = FALSE;
  BOOL daclDefaulted = FALSE;
  if (!GetSecurityDescriptorOwner(expected.get(), &expectedOwner, &ownerDefaulted) ||
      !expectedOwner || !IsValidSid(expectedOwner) ||
      !GetSecurityDescriptorDacl(
          expected.get(), &daclPresent, &expectedDacl, &daclDefaulted) ||
      !daclPresent || !expectedDacl || !IsValidAcl(expectedDacl)) {
    throw Error("health security template is invalid");
  }
  DWORD code = SetSecurityInfo(
      object,
      SE_FILE_OBJECT,
      DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
      nullptr,
      nullptr,
      expectedDacl,
      nullptr);
  if (code != ERROR_SUCCESS) failWin("SetSecurityInfo(health DACL)", code);
  const DWORD ownerCode = SetSecurityInfo(
      object,
      SE_FILE_OBJECT,
      OWNER_SECURITY_INFORMATION,
      expectedOwner,
      nullptr,
      nullptr,
      nullptr);
  if (ownerCode != ERROR_SUCCESS &&
      ownerCode != ERROR_INVALID_OWNER &&
      ownerCode != ERROR_ACCESS_DENIED &&
      ownerCode != ERROR_PRIVILEGE_NOT_HELD) {
    failWin("SetSecurityInfo(health owner)", ownerCode);
  }

  PSECURITY_DESCRIPTOR rawActual = nullptr;
  PSID actualOwner = nullptr;
  PACL actualDacl = nullptr;
  code = GetSecurityInfo(
      object,
      SE_FILE_OBJECT,
      OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
      &actualOwner,
      nullptr,
      &actualDacl,
      nullptr,
      &rawActual);
  if (code != ERROR_SUCCESS) failWin("GetSecurityInfo(health file)", code);
  Local<SECURITY_DESCRIPTOR> actual(static_cast<SECURITY_DESCRIPTOR*>(rawActual));
  SECURITY_DESCRIPTOR_CONTROL control = 0;
  DWORD revision = 0;
  if (!GetSecurityDescriptorControl(actual.get(), &control, &revision)) {
    failWin("GetSecurityDescriptorControl(health file)");
  }
  if (!actualOwner || !IsValidSid(actualOwner) ||
      !(control & SE_DACL_PROTECTED) ||
      !exactAcl(expectedDacl, actualDacl)) {
    throw Error("health file owner or protected DACL verification failed");
  }
  const bool assigned = EqualSid(expectedOwner, actualOwner) != FALSE;
  if (ownerCode == ERROR_SUCCESS && !assigned) {
    throw Error("health file owner assignment did not round-trip");
  }
  return assigned;
}

void protectServiceHealth(const std::vector<std::wstring>& args) {
  expect(args, 2, "protect-service-health <path> <worker|coordinator>");
  const wchar_t* roleAccount = nullptr;
  if (args[1] == L"worker") {
    roleAccount = kWorkerServiceAccount;
  } else if (args[1] == L"coordinator") {
    roleAccount = kCoordinatorServiceAccount;
  } else {
    throw Error("service health role is not allowlisted");
  }
  const auto roleSidBytes = serviceSidForAccount(roleAccount);
  const auto updaterSidBytes = serviceSidForName(kUpdaterServiceName);
  const std::wstring roleSid =
      sidText(const_cast<std::uint8_t*>(roleSidBytes.data()));
  const std::wstring updaterSid =
      sidText(const_cast<std::uint8_t*>(updaterSidBytes.data()));
  std::wstring sddl = L"O:" + roleSid + L"D:P";
  appendDirectoryAllow(sddl, false, L"FA", L"SY");
  appendDirectoryAllow(sddl, false, L"FA", L"BA");
  appendDirectoryAllow(sddl, false, L"GR", roleSid);
  appendDirectoryAllow(sddl, false, L"GR", updaterSid);
  appendDirectoryAllow(sddl, false, L"GRGX", L"S-1-3-4");

  Handle file(CreateFileW(
      args[0].c_str(),
      READ_CONTROL | WRITE_DAC | WRITE_OWNER,
      FILE_SHARE_READ | FILE_SHARE_WRITE,
      nullptr,
      OPEN_EXISTING,
      FILE_FLAG_OPEN_REPARSE_POINT,
      nullptr));
  if (!file) failWin("CreateFileW(service health)");
  FILE_ATTRIBUTE_TAG_INFO attributes{};
  if (!GetFileInformationByHandleEx(
      file.get(), FileAttributeTagInfo, &attributes, sizeof(attributes))) {
    failWin("GetFileInformationByHandleEx(service health)");
  }
  if (attributes.FileAttributes &
      (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)) {
    throw Error("service health path is not a regular non-reparse file");
  }
  const bool ownerAssigned =
      setAndVerifyFileSecurityWithOptionalOwner(file.get(), sddl);
  emit(std::string("{\"protected\":true,\"ownerAssigned\":") +
      (ownerAssigned ? "true}" : "false}"));
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

void applyAccountDacl(const std::wstring& path, const std::wstring& account) {
  auto current = currentSid();
  auto service = lookupAccountSid(account);
  std::wstring sddl = L"D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;" +
      sidText(current.data()) + L")(A;;FA;;;" + sidText(service.data()) + L")";
  PSECURITY_DESCRIPTOR raw = nullptr;
  if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
      sddl.c_str(), SDDL_REVISION_1, &raw, nullptr)) {
    failWin("ConvertStringSecurityDescriptorToSecurityDescriptorW");
  }
  Local<SECURITY_DESCRIPTOR> descriptor(static_cast<SECURITY_DESCRIPTOR*>(raw));
  BOOL present = FALSE;
  BOOL defaulted = FALSE;
  PACL dacl = nullptr;
  if (!GetSecurityDescriptorDacl(descriptor.get(), &present, &dacl, &defaulted) || !present) {
    failWin("GetSecurityDescriptorDacl");
  }
  DWORD code = SetNamedSecurityInfoW(
      const_cast<wchar_t*>(path.c_str()),
      SE_FILE_OBJECT,
      DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
      nullptr,
      nullptr,
      dacl,
      nullptr);
  if (code != ERROR_SUCCESS) failWin("SetNamedSecurityInfoW", code);
}

void requireUpdaterOrElevatedInstallerContext(const char* operation);

void applyArtifactDacl(
    const std::wstring& path,
    const std::wstring& writerAccount = L"") {
  auto current = currentSid();
  std::vector<std::uint8_t> writer;
  const bool updaterWriter =
      !writerAccount.empty() && equalOrdinalIgnoreCase(writerAccount, kUpdaterServiceAccount);
  if (updaterWriter) {
    requireUpdaterOrElevatedInstallerContext("apply-artifact-dacl");
  }
  if (!writerAccount.empty()) {
    writer = updaterWriter
        ? serviceSidForName(kUpdaterServiceName)
        : lookupAccountSid(writerAccount);
  }
  const std::wstring writerSid = writer.empty() ? L"" : sidText(writer.data());
  const wchar_t* currentRights = writerAccount.empty() ? L"FA" : L"GRGX";
  std::wstring sddl = updaterWriter ? L"O:" + writerSid + L"D:P" : L"D:P";
  sddl += L"(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;GRGX;;;S-1-3-4)(A;OICI;" +
      std::wstring(currentRights) + L";;;" + sidText(current.data()) + L")";
  if (!writer.empty()) {
    sddl += L"(A;OICI;FA;;;" + writerSid + L")";
  }
  DWORD readerChars = GetEnvironmentVariableW(L"ROOST_INTERACTIVE_SID", nullptr, 0);
  if (readerChars > 0) {
    std::vector<wchar_t> readerText(readerChars);
    if (GetEnvironmentVariableW(
        L"ROOST_INTERACTIVE_SID", readerText.data(), readerChars) + 1 != readerChars) {
      failWin("GetEnvironmentVariableW(ROOST_INTERACTIVE_SID)");
    }
    const auto reader = sidFromText(readerText.data(), "ROOST_INTERACTIVE_SID");
    sddl += L"(A;OICI;GRGX;;;" +
        sidText(const_cast<std::uint8_t*>(reader.data())) + L")";
  } else if (GetLastError() != ERROR_ENVVAR_NOT_FOUND) {
    failWin("GetEnvironmentVariableW(ROOST_INTERACTIVE_SID)");
  }

  if (updaterWriter) {
    Handle object(CreateFileW(
        path.c_str(),
        READ_CONTROL | WRITE_DAC | WRITE_OWNER,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        nullptr,
        OPEN_EXISTING,
        FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
        nullptr));
    if (!object) failWin("CreateFileW(updater artifact)");
    FILE_ATTRIBUTE_TAG_INFO attributes{};
    if (!GetFileInformationByHandleEx(
        object.get(), FileAttributeTagInfo, &attributes, sizeof(attributes))) {
      failWin("GetFileInformationByHandleEx(updater artifact)");
    }
    if (attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) {
      throw Error("updater artifact is a reparse point");
    }
    setAndVerifyFileSecurity(object.get(), sddl);
    return;
  }

  PSECURITY_DESCRIPTOR raw = nullptr;
  if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
      sddl.c_str(), SDDL_REVISION_1, &raw, nullptr)) {
    failWin("ConvertStringSecurityDescriptorToSecurityDescriptorW");
  }
  Local<SECURITY_DESCRIPTOR> descriptor(static_cast<SECURITY_DESCRIPTOR*>(raw));
  BOOL present = FALSE;
  BOOL defaulted = FALSE;
  PACL dacl = nullptr;
  if (!GetSecurityDescriptorDacl(descriptor.get(), &present, &dacl, &defaulted) || !present) {
    failWin("GetSecurityDescriptorDacl");
  }
  DWORD code = SetNamedSecurityInfoW(
      const_cast<wchar_t*>(path.c_str()),
      SE_FILE_OBJECT,
      DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
      nullptr,
      nullptr,
      dacl,
      nullptr);
  if (code != ERROR_SUCCESS) failWin("SetNamedSecurityInfoW", code);
}

void flushFile(const std::vector<std::wstring>& args) {
  expect(args, 1, "flush-file <path>");
  DWORD attributes = GetFileAttributesW(args[0].c_str());
  if (attributes == INVALID_FILE_ATTRIBUTES) failWin("GetFileAttributesW");
  DWORD flags = (attributes & FILE_ATTRIBUTE_DIRECTORY)
      ? FILE_FLAG_BACKUP_SEMANTICS
      : FILE_ATTRIBUTE_NORMAL;
  Handle file(CreateFileW(args[0].c_str(), GENERIC_WRITE,
      FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr, OPEN_EXISTING,
      flags, nullptr));
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

void applyArtifactDaclCommand(const std::vector<std::wstring>& args) {
  if (args.size() != 1 && args.size() != 2) {
    throw Error("usage: apply-artifact-dacl <path> [writer-account]");
  }
  applyArtifactDacl(args[0], args.size() == 2 ? args[1] : L"");
  emit("{\"ok\":true}");
}

void applyAccountDaclCommand(const std::vector<std::wstring>& args) {
  expect(args, 2, "apply-account-dacl <path> <account>");
  applyAccountDacl(args[0], args[1]);
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

struct AuthenticodeProof {
  std::string publisherSha256;
  bool timestamped = false;
};

AuthenticodeProof inspectAuthenticode(
    const std::wstring& path,
    const std::string& expectedHash) {
  WINTRUST_FILE_INFO file{};
  file.cbStruct = sizeof(file);
  file.pcwszFilePath = path.c_str();
  WINTRUST_DATA trust{};
  trust.cbStruct = sizeof(trust);
  trust.dwUIChoice = WTD_UI_NONE;
  trust.fdwRevocationChecks = WTD_REVOKE_WHOLECHAIN;
  trust.dwUnionChoice = WTD_CHOICE_FILE;
  trust.pFile = &file;
  trust.dwStateAction = WTD_STATEACTION_VERIFY;
  trust.dwProvFlags = WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT;
  GUID action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
  const LONG result = WinVerifyTrust(nullptr, &action, &trust);
  bool valid = result == ERROR_SUCCESS;
  AuthenticodeProof proof;
  if (valid) {
    CRYPT_PROVIDER_DATA* data = WTHelperProvDataFromStateData(trust.hWVTStateData);
    CRYPT_PROVIDER_SGNR* signer =
        data ? WTHelperGetProvSignerFromChain(data, 0, FALSE, 0) : nullptr;
    if (!signer || !signer->csCertChain || !signer->pasCertChain[0].pCert) {
      valid = false;
    } else {
      proof.publisherSha256 = hexHash(certificateHash(signer->pasCertChain[0].pCert));
      proof.timestamped = signer->csCounterSigners > 0;
      valid = proof.publisherSha256 == expectedHash && proof.timestamped;
    }
  }
  trust.dwStateAction = WTD_STATEACTION_CLOSE;
  WinVerifyTrust(nullptr, &action, &trust);
  if (!valid) {
    throw Error("Authenticode verification, timestamp, or publisher pin failed");
  }
  return proof;
}

void verifyAuthenticode(const std::vector<std::wstring>& args) {
  if (args.size() != 3 || args[1] != L"--publisher-sha256") {
    throw Error("usage: verify-authenticode <asset> --publisher-sha256 <sha256>");
  }
  const std::string expectedHash = checkedPublisher(args[2]);
  const AuthenticodeProof proof = inspectAuthenticode(args[0], expectedHash);
  emit("{\"valid\":true,\"publisherSha256\":" + json(proof.publisherSha256) +
      ",\"timestamped\":true}");
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
class FindHandle final {
 public:
  explicit FindHandle(HANDLE value = INVALID_HANDLE_VALUE) : value_(value) {}
  ~FindHandle() {
    if (value_ != INVALID_HANDLE_VALUE) FindClose(value_);
  }
  FindHandle(const FindHandle&) = delete;
  FindHandle& operator=(const FindHandle&) = delete;
  HANDLE get() const { return value_; }
  explicit operator bool() const { return value_ != INVALID_HANDLE_VALUE; }
 private:
  HANDLE value_;
};
std::wstring finalDosPath(HANDLE object, const char* label) {
  DWORD needed = GetFinalPathNameByHandleW(
      object, nullptr, 0, FILE_NAME_NORMALIZED | VOLUME_NAME_DOS);
  if (!needed) failWin(label);
  std::wstring value(needed, L'\0');
  const DWORD written = GetFinalPathNameByHandleW(
      object,
      value.data(),
      static_cast<DWORD>(value.size()),
      FILE_NAME_NORMALIZED | VOLUME_NAME_DOS);
  if (!written || written >= value.size()) failWin(label);
  value.resize(written);
  static const std::wstring localPrefix = L"\\\\?\\";
  static const std::wstring uncPrefix = L"\\\\?\\UNC\\";
  if (value.size() >= uncPrefix.size() &&
      equalOrdinalIgnoreCase(value.substr(0, uncPrefix.size()), uncPrefix)) {
    throw Error("trusted tree must remain on a local volume");
  }
  if (value.size() >= localPrefix.size() &&
      equalOrdinalIgnoreCase(value.substr(0, localPrefix.size()), localPrefix)) {
    value.erase(0, localPrefix.size());
  }
  return fullPath(value);
}
bool pathAtOrBelow(const std::wstring& root, const std::wstring& candidate);
std::wstring objectSecuritySddl(HANDLE object) {
  PSECURITY_DESCRIPTOR raw = nullptr;
  PSID owner = nullptr;
  PACL dacl = nullptr;
  const DWORD code = GetSecurityInfo(
      object, SE_FILE_OBJECT,
      OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
      &owner, nullptr, &dacl, nullptr, &raw);
  if (code != ERROR_SUCCESS) failWin("GetSecurityInfo(file security)", code);
  Local<SECURITY_DESCRIPTOR> descriptor(
      static_cast<SECURITY_DESCRIPTOR*>(raw));
  BOOL present = FALSE;
  BOOL defaulted = FALSE;
  PACL descriptorDacl = nullptr;
  SECURITY_DESCRIPTOR_CONTROL control = 0;
  DWORD revision = 0;
  if (!owner || !IsValidSid(owner) ||
      !GetSecurityDescriptorDacl(
          descriptor.get(), &present, &descriptorDacl, &defaulted) ||
      !present || !descriptorDacl || descriptorDacl != dacl ||
      !IsValidAcl(dacl) ||
      !GetSecurityDescriptorControl(descriptor.get(), &control, &revision)) {
    throw Error("file security descriptor is incomplete or invalid");
  }
  wchar_t* rawText = nullptr;
  if (!ConvertSecurityDescriptorToStringSecurityDescriptorW(
          descriptor.get(), SDDL_REVISION_1,
          OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
          &rawText, nullptr)) {
    failWin("ConvertSecurityDescriptorToStringSecurityDescriptorW(file)");
  }
  Local<wchar_t> text(rawText);
  return text.get();
}

struct SecurityTreeEntry {
  std::wstring path;
  std::wstring relative;
  std::wstring sddl;
  bool directory = false;
  DWORD linkCount = 0;
  Handle handle;
};

std::vector<SecurityTreeEntry> openSecurityTree(
    const std::wstring& inputRoot,
    ACCESS_MASK access) {
  const std::wstring root = fullPath(inputRoot);
  ensureSafeParent(parentPath(root));
  struct Pending {
    std::wstring path;
    std::wstring relative;
  };
  std::vector<Pending> pending{{root, L""}};
  std::vector<SecurityTreeEntry> entries;
  std::wstring trustedRoot;
  DWORD trustedVolume = 0;
  while (!pending.empty()) {
    Pending next = std::move(pending.back());
    pending.pop_back();
    Handle object(CreateFileW(
        next.path.c_str(), access, FILE_SHARE_READ, nullptr, OPEN_EXISTING,
        FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS, nullptr));
    if (!object) failWin("CreateFileW(file security tree)");
    FILE_ATTRIBUTE_TAG_INFO attributes{};
    BY_HANDLE_FILE_INFORMATION information{};
    if (!GetFileInformationByHandleEx(
            object.get(), FileAttributeTagInfo, &attributes, sizeof(attributes)) ||
        !GetFileInformationByHandle(object.get(), &information)) {
      failWin("GetFileInformationByHandle(file security tree)");
    }
    if (attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) {
      throw Error("file security tree contains a reparse point");
    }
    const bool directory =
        (attributes.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0;
    if (!directory && information.nNumberOfLinks != 1) {
      throw Error("file security tree contains a multiply-linked file");
    }
    const std::wstring resolved =
        finalDosPath(object.get(), "GetFinalPathNameByHandleW(file security tree)");
    if (entries.empty()) {
      if (!directory) throw Error("file security tree root is not a directory");
      trustedRoot = resolved;
      trustedVolume = information.dwVolumeSerialNumber;
    } else if (
        information.dwVolumeSerialNumber != trustedVolume ||
        !pathAtOrBelow(trustedRoot, resolved)) {
      throw Error("file security tree object escaped its trusted root");
    }
    entries.push_back({
        std::move(next.path), std::move(next.relative),
        objectSecuritySddl(object.get()), directory,
        information.nNumberOfLinks, std::move(object),
    });
    if (entries.size() > 8192) {
      throw Error("file security tree contains too many objects");
    }
    if (!directory) continue;
    WIN32_FIND_DATAW data{};
    FindHandle search(
        FindFirstFileW((entries.back().path + L"\\*").c_str(), &data));
    if (!search) failWin("FindFirstFileW(file security tree)");
    do {
      if (!std::wcscmp(data.cFileName, L".") ||
          !std::wcscmp(data.cFileName, L"..")) continue;
      if (data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) {
        throw Error("file security tree contains a reparse point");
      }
      const std::wstring relative = entries.back().relative.empty()
          ? std::wstring(data.cFileName)
          : entries.back().relative + L"\\" + data.cFileName;
      pending.push_back({
          entries.back().path + L"\\" + data.cFileName, relative});
    } while (FindNextFileW(search.get(), &data));
    if (GetLastError() != ERROR_NO_MORE_FILES) {
      failWin("FindNextFileW(file security tree)");
    }
  }
  return entries;
}

void snapshotFileSecurityTree(const std::vector<std::wstring>& args) {
  expect(args, 1, "snapshot-file-security-tree <root>");
  std::vector<SecurityTreeEntry> entries =
      openSecurityTree(args[0], READ_CONTROL);
  std::string out = "{\"entries\":[";
  for (std::size_t index = 0; index < entries.size(); ++index) {
    if (index) out.push_back(',');
    const SecurityTreeEntry& entry = entries[index];
    out += "{\"relativePath\":" + json(entry.relative) +
        ",\"kind\":" + json(entry.directory ? "directory" : "file") +
        ",\"linkCount\":" + std::to_string(entry.linkCount) +
        ",\"sddl\":" + json(entry.sddl) + "}";
    if (out.size() > 3U * 1024U * 1024U) {
      throw Error("file security tree snapshot is too large");
    }
  }
  out += "]}";
  emit(std::move(out));
}

void restoreFileSecurityTree(const std::vector<std::wstring>& args) {
  expect(args, 1, "restore-file-security-tree <root>");
  const std::vector<std::uint8_t> input = framedInput();
  const JsonValue root = JsonParser(std::string_view(
      reinterpret_cast<const char*>(input.data()), input.size())).parse();
  if (root.type != JsonValue::Type::Object ||
      root.object.size() != 1 || !root.object.contains("entries")) {
    throw Error("file security tree snapshot must contain only entries");
  }
  const JsonValue& values = member(root, "entries", JsonValue::Type::Array);
  if (values.array.empty() || values.array.size() > 8192) {
    throw Error("file security tree snapshot entry count is invalid");
  }
  struct Expected {
    std::wstring relative;
    std::wstring canonical;
    std::wstring sddl;
    bool directory = false;
    DWORD linkCount = 0;
  };
  std::vector<Expected> expected;
  std::set<std::wstring> seen;
  for (const JsonValue& value : values.array) {
    if (value.type != JsonValue::Type::Object ||
        value.object.size() != 4 ||
        !value.object.contains("relativePath") ||
        !value.object.contains("kind") ||
        !value.object.contains("linkCount") ||
        !value.object.contains("sddl")) {
      throw Error("file security tree entry has unknown or missing fields");
    }
    const std::string& rawRelative =
        member(value, "relativePath", JsonValue::Type::String).string;
    std::wstring relative;
    std::wstring canonical;
    if (!rawRelative.empty()) {
      const NormalPath normalized = normalizeArchivePath(rawRelative, true);
      if (normalized.directory) {
        throw Error("file security relativePath must not end in a slash");
      }
      relative = normalized.display;
      canonical = normalized.canonical;
    }
    if (!seen.insert(canonical).second) {
      throw Error("file security tree snapshot contains duplicate paths");
    }
    const std::string& kind =
        member(value, "kind", JsonValue::Type::String).string;
    if (kind != "file" && kind != "directory") {
      throw Error("file security tree entry kind is invalid");
    }
    const std::uint64_t links =
        member(value, "linkCount", JsonValue::Type::Number).number;
    if (!links || links > std::numeric_limits<DWORD>::max()) {
      throw Error("file security tree entry linkCount is invalid");
    }
    const std::wstring sddl = fromUtf8(
        member(value, "sddl", JsonValue::Type::String).string);
    if (sddl.empty() || sddl.size() > 32768) {
      throw Error("file security tree entry SDDL is invalid");
    }
    expected.push_back({
        std::move(relative), std::move(canonical), sddl,
        kind == "directory", static_cast<DWORD>(links)});
  }
  const auto rootExpected = std::find_if(
      expected.begin(), expected.end(),
      [](const Expected& entry) { return entry.relative.empty(); });
  if (rootExpected == expected.end() || !rootExpected->directory) {
    throw Error("file security tree snapshot has no directory root");
  }

  std::vector<SecurityTreeEntry> actual = openSecurityTree(
      args[0], READ_CONTROL | WRITE_DAC | WRITE_OWNER);
  if (actual.size() != expected.size()) {
    throw Error("file security tree changed since its snapshot");
  }
  std::map<std::wstring, std::size_t> actualByPath;
  for (std::size_t index = 0; index < actual.size(); ++index) {
    std::wstring canonical;
    if (!actual[index].relative.empty()) {
      canonical = normalizeArchivePath(
          toUtf8(actual[index].relative), true).canonical;
    }
    if (!actualByPath.emplace(canonical, index).second) {
      throw Error("file security tree contains duplicate canonical paths");
    }
  }
  std::vector<std::pair<std::size_t, std::size_t>> order;
  for (std::size_t index = 0; index < expected.size(); ++index) {
    const auto found = actualByPath.find(expected[index].canonical);
    if (found == actualByPath.end()) {
      throw Error("file security tree path set changed since its snapshot");
    }
    const SecurityTreeEntry& entry = actual[found->second];
    if (entry.directory != expected[index].directory ||
        entry.linkCount != expected[index].linkCount) {
      throw Error("file security tree metadata changed since its snapshot");
    }
    const std::size_t depth = static_cast<std::size_t>(std::count(
        expected[index].relative.begin(),
        expected[index].relative.end(), L'\\'));
    order.emplace_back(depth, index);
  }
  std::stable_sort(order.begin(), order.end());
  for (const auto& item : order) {
    const std::size_t expectedIndex = item.second;
    const std::size_t actualIndex =
        actualByPath.at(expected[expectedIndex].canonical);
    setAndVerifyFileSecurity(
        actual[actualIndex].handle.get(), expected[expectedIndex].sddl);
  }
  emit("{\"restored\":true,\"entries\":" +
      std::to_string(actual.size()) + "}");
}


bool pathAtOrBelow(const std::wstring& root, const std::wstring& candidate) {
  if (equalOrdinalIgnoreCase(root, candidate)) return true;
  std::wstring prefix = root;
  if (prefix.empty() || (prefix.back() != L'\\' && prefix.back() != L'/')) {
    prefix.push_back(L'\\');
  }
  return candidate.size() > prefix.size() &&
      equalOrdinalIgnoreCase(candidate.substr(0, prefix.size()), prefix);
}
struct UpdaterArtifactRoots {
  std::wstring service;
  std::wstring updater;
  std::wstring versions;
  std::wstring bin;
  std::wstring requests;
  std::wstring localRequests;
};
std::wstring trustedUpdaterInstallRoot();
std::optional<std::wstring> authorizedInstallerInstallRoot();
bool updaterRequestName(const std::wstring& name);


std::wstring requiredLocalEnvironmentPath(const wchar_t* name) {
  const auto value = processEnvironmentValue(name);
  if (!value || value->size() < 3 || !std::iswalpha((*value)[0]) ||
      (*value)[1] != L':' ||
      ((*value)[2] != L'\\' && (*value)[2] != L'/')) {
    throw Error("required updater path environment is not absolute and local");
  }
  return fullPath(*value);
}

UpdaterArtifactRoots updaterArtifactRoots() {
  const std::optional<std::wstring> installerRoot =
      authorizedInstallerInstallRoot();
  const std::wstring installRoot =
      installerRoot ? *installerRoot : trustedUpdaterInstallRoot();
  const std::wstring service = fullPath(installRoot + L"\\service");
  const std::wstring versions = fullPath(installRoot + L"\\versions");
  const auto serviceValue = processEnvironmentValue(L"ROOST_SERVICE_DIR");
  if (serviceValue && !serviceValue->empty() &&
      !equalOrdinalIgnoreCase(
          requiredLocalEnvironmentPath(L"ROOST_SERVICE_DIR"), service)) {
    throw Error("ROOST_SERVICE_DIR disagrees with the trusted install root");
  }
  const auto versionsValue = processEnvironmentValue(L"ROOST_VERSIONS_DIR");
  if (versionsValue && !versionsValue->empty() &&
      !equalOrdinalIgnoreCase(
          requiredLocalEnvironmentPath(L"ROOST_VERSIONS_DIR"), versions)) {
    throw Error("ROOST_VERSIONS_DIR disagrees with the trusted install root");
  }
  ensureSafeParent(service);
  ensureSafeParent(versions);
  const std::wstring updater = fullPath(service + L"\\data\\updater");
  const std::wstring bin = fullPath(installRoot + L"\\bin");
  const std::wstring requests = fullPath(service + L"\\requests");
  const std::wstring localRequests =
      fullPath(requests + L"\\interactive-update");
  return {service, updater, versions, bin, requests, localRequests};
}

void requireUpdaterArtifactProfile(const std::wstring& profile) {
  if (profile != L"private" && profile != L"control" &&
      profile != L"status" && profile != L"current" &&
      profile != L"release" && profile != L"stable-shawl" &&
      profile != L"stable-launcher") {
    throw Error("updater artifact profile is not allowlisted");
  }
}

std::wstring updaterArtifactSddl(
    const std::wstring& profile,
    PSID owner,
    bool directory) {
  requireUpdaterArtifactProfile(profile);
  if (!owner || !IsValidSid(owner)) {
    throw Error("updater artifact owner is invalid");
  }
  const auto keeper = serviceSidForName(kKeeperServiceName);
  const auto worker = serviceSidForName(kWorkerServiceName);
  const auto coordinator = serviceSidForName(kCoordinatorServiceName);
  const auto updater = serviceSidForName(kUpdaterServiceName);
  std::wstring sddl = L"O:" + sidText(owner) + L"D:P";
  appendDirectoryAllow(sddl, directory, L"FA", L"SY");
  appendDirectoryAllow(sddl, directory, L"FA", L"BA");
  appendDirectoryAllow(sddl, directory, L"RC", L"S-1-3-4");
  appendDirectoryAllow(
      sddl, directory, L"FA",
      sidText(const_cast<std::uint8_t*>(updater.data())));
  if (profile == L"control" || profile == L"status") {
    appendDirectoryAllow(
        sddl, directory, L"GR",
        sidText(const_cast<std::uint8_t*>(worker.data())));
    appendDirectoryAllow(
        sddl, directory, L"GR",
        sidText(const_cast<std::uint8_t*>(coordinator.data())));
    if (profile == L"status") {
      appendDirectoryAllow(sddl, directory, L"GR", L"BU");
    }
  } else if (profile == L"current" || profile == L"release" ||
      profile == L"stable-shawl" || profile == L"stable-launcher") {
    const wchar_t* rights = profile == L"current" ? L"GR" : L"GRGX";
    appendDirectoryAllow(
        sddl, directory, rights,
        sidText(const_cast<std::uint8_t*>(keeper.data())));
    appendDirectoryAllow(
        sddl, directory, rights,
        sidText(const_cast<std::uint8_t*>(worker.data())));
    appendDirectoryAllow(
        sddl, directory, rights,
        sidText(const_cast<std::uint8_t*>(coordinator.data())));
    const auto interactiveValue =
        processEnvironmentValue(L"ROOST_INTERACTIVE_SID");
    if (!interactiveValue || interactiveValue->empty()) {
      throw Error("ROOST_INTERACTIVE_SID is required for shared updater artifacts");
    }
    const auto interactive =
        sidFromText(*interactiveValue, "ROOST_INTERACTIVE_SID");
    appendDirectoryAllow(
        sddl, directory, rights,
        sidText(const_cast<std::uint8_t*>(interactive.data())));
  }
  return sddl;
}

void requireExactFileSecurity(HANDLE object, const std::wstring& sddl) {
  PSECURITY_DESCRIPTOR rawExpected = nullptr;
  if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
          sddl.c_str(), SDDL_REVISION_1, &rawExpected, nullptr)) {
    failWin("ConvertStringSecurityDescriptorToSecurityDescriptorW(expected)");
  }
  Local<SECURITY_DESCRIPTOR> expected(
      static_cast<SECURITY_DESCRIPTOR*>(rawExpected));
  PSID expectedOwner = nullptr;
  PACL expectedDacl = nullptr;
  BOOL present = FALSE;
  BOOL defaulted = FALSE;
  SECURITY_DESCRIPTOR_CONTROL expectedControl = 0;
  DWORD revision = 0;
  if (!GetSecurityDescriptorOwner(
          expected.get(), &expectedOwner, &defaulted) ||
      !expectedOwner ||
      !GetSecurityDescriptorDacl(
          expected.get(), &present, &expectedDacl, &defaulted) ||
      !present || !expectedDacl ||
      !GetSecurityDescriptorControl(
          expected.get(), &expectedControl, &revision)) {
    throw Error("expected file security descriptor is invalid");
  }

  PSECURITY_DESCRIPTOR rawActual = nullptr;
  PSID actualOwner = nullptr;
  PACL actualDacl = nullptr;
  DWORD code = GetSecurityInfo(
      object, SE_FILE_OBJECT,
      OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
      &actualOwner, nullptr, &actualDacl, nullptr, &rawActual);
  if (code != ERROR_SUCCESS) failWin("GetSecurityInfo(exact file)", code);
  Local<SECURITY_DESCRIPTOR> actual(
      static_cast<SECURITY_DESCRIPTOR*>(rawActual));
  SECURITY_DESCRIPTOR_CONTROL actualControl = 0;
  if (!GetSecurityDescriptorControl(
          actual.get(), &actualControl, &revision) ||
      !actualOwner || !EqualSid(expectedOwner, actualOwner) ||
      ((actualControl & SE_DACL_PROTECTED) !=
       (expectedControl & SE_DACL_PROTECTED)) ||
      !exactAcl(expectedDacl, actualDacl)) {
    throw Error("updater artifact security descriptor is not exact");
  }
}
bool fileSecurityMatchesExact(HANDLE object, const std::wstring& sddl) {
  PSECURITY_DESCRIPTOR rawExpected = nullptr;
  if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
          sddl.c_str(), SDDL_REVISION_1, &rawExpected, nullptr)) {
    failWin(
        "ConvertStringSecurityDescriptorToSecurityDescriptorW(comparison)");
  }
  Local<SECURITY_DESCRIPTOR> expected(
      static_cast<SECURITY_DESCRIPTOR*>(rawExpected));
  PSID expectedOwner = nullptr;
  PACL expectedDacl = nullptr;
  BOOL present = FALSE;
  BOOL defaulted = FALSE;
  SECURITY_DESCRIPTOR_CONTROL expectedControl = 0;
  DWORD revision = 0;
  if (!GetSecurityDescriptorOwner(
          expected.get(), &expectedOwner, &defaulted) ||
      !expectedOwner ||
      !GetSecurityDescriptorDacl(
          expected.get(), &present, &expectedDacl, &defaulted) ||
      !present || !expectedDacl ||
      !GetSecurityDescriptorControl(
          expected.get(), &expectedControl, &revision)) {
    throw Error("comparison file security descriptor is invalid");
  }
  PSECURITY_DESCRIPTOR rawActual = nullptr;
  PSID actualOwner = nullptr;
  PACL actualDacl = nullptr;
  const DWORD code = GetSecurityInfo(
      object, SE_FILE_OBJECT,
      OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
      &actualOwner, nullptr, &actualDacl, nullptr, &rawActual);
  if (code != ERROR_SUCCESS) {
    failWin("GetSecurityInfo(file comparison)", code);
  }
  Local<SECURITY_DESCRIPTOR> actual(
      static_cast<SECURITY_DESCRIPTOR*>(rawActual));
  SECURITY_DESCRIPTOR_CONTROL actualControl = 0;
  if (!GetSecurityDescriptorControl(
          actual.get(), &actualControl, &revision) ||
      !actualOwner) {
    throw Error("actual file security descriptor is invalid");
  }
  return EqualSid(expectedOwner, actualOwner) &&
      ((actualControl & SE_DACL_PROTECTED) ==
       (expectedControl & SE_DACL_PROTECTED)) &&
      exactAcl(expectedDacl, actualDacl);
}


bool updaterStatusArtifact(
    const std::wstring& path,
    const UpdaterArtifactRoots& roots);

void validateUpdaterArtifactPath(
    const std::wstring& path,
    const std::wstring& profile,
    const UpdaterArtifactRoots& roots) {
  requireUpdaterArtifactProfile(profile);
  const std::wstring canonical = fullPath(path);
  const bool allowed =
      profile == L"current"
      ? equalOrdinalIgnoreCase(
            canonical, fullPath(roots.service + L"\\current.json"))
      : profile == L"stable-shawl"
      ? equalOrdinalIgnoreCase(
            canonical, fullPath(roots.bin + L"\\shawl.exe"))
      : profile == L"stable-launcher"
      ? equalOrdinalIgnoreCase(
            canonical, fullPath(roots.bin + L"\\roost.exe"))
      : profile == L"status"
      ? updaterStatusArtifact(canonical, roots)
      : profile == L"control"
      ? pathAtOrBelow(roots.updater, canonical) ||
            equalOrdinalIgnoreCase(
                canonical,
                fullPath(roots.service + L"\\update-v2.json")) ||
            equalOrdinalIgnoreCase(
                canonical,
                fullPath(roots.service + L"\\update-v1.json")) ||
            equalOrdinalIgnoreCase(
                canonical,
                fullPath(roots.service + L"\\service-definitions.json"))
      : profile == L"release"
      ? pathAtOrBelow(roots.versions, canonical) ||
            pathAtOrBelow(roots.bin, canonical)
      : pathAtOrBelow(roots.service, canonical) ||
            pathAtOrBelow(roots.versions, canonical) ||
            pathAtOrBelow(roots.bin, canonical);
  if (!allowed) throw Error("updater artifact escaped its trusted roots");
  ensureSafeParent(parentPath(canonical));
}

struct OpenUpdaterArtifact {
  Handle handle;
  std::wstring sddl;
  bool directory = false;
};

OpenUpdaterArtifact openUpdaterArtifact(
    const std::wstring& path,
    const std::wstring& profile,
    ACCESS_MASK access,
    DWORD share = FILE_SHARE_READ,
    bool allowOwnerRepair = false) {
  const UpdaterArtifactRoots roots = updaterArtifactRoots();
  const std::wstring canonical = fullPath(path);
  validateUpdaterArtifactPath(canonical, profile, roots);
  Handle object(CreateFileW(
      canonical.c_str(), access, share, nullptr, OPEN_EXISTING,
      FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS, nullptr));
  if (!object) failWin("CreateFileW(updater artifact)");
  FILE_ATTRIBUTE_TAG_INFO attributes{};
  BY_HANDLE_FILE_INFORMATION information{};
  if (!GetFileInformationByHandleEx(
          object.get(), FileAttributeTagInfo, &attributes, sizeof(attributes)) ||
      !GetFileInformationByHandle(object.get(), &information)) {
    failWin("GetFileInformationByHandle(updater artifact)");
  }
  if (attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) {
    throw Error("updater artifact is a reparse point");
  }
  const bool directory =
      (attributes.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0;
  if (!directory && information.nNumberOfLinks != 1) {
    throw Error("updater artifact is multiply linked");
  }
  const std::wstring finalPath =
      finalDosPath(object.get(), "GetFinalPathNameByHandleW(updater artifact)");
  if (!equalOrdinalIgnoreCase(finalPath, canonical)) {
    throw Error("updater artifact resolved to a different final path");
  }
  validateUpdaterArtifactPath(finalPath, profile, roots);
  PSECURITY_DESCRIPTOR raw = nullptr;
  PSID owner = nullptr;
  DWORD code = GetSecurityInfo(
      object.get(), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION,
      &owner, nullptr, nullptr, nullptr, &raw);
  if (code != ERROR_SUCCESS) failWin("GetSecurityInfo(updater owner)", code);
  Local<SECURITY_DESCRIPTOR> descriptor(
      static_cast<SECURITY_DESCRIPTOR*>(raw));
  const std::wstring requestParent = parentPath(finalPath);
  const bool requestInboxException =
      profile == L"private" &&
      (equalOrdinalIgnoreCase(requestParent, roots.requests) ||
       equalOrdinalIgnoreCase(requestParent, roots.localRequests)) &&
      updaterRequestName(baseName(finalPath));
  auto updaterOwner = serviceSidForAccount(kUpdaterServiceAccount);
  PSID expectedOwner = requestInboxException
      ? owner
      : static_cast<PSID>(updaterOwner.data());
  if (!owner || !IsValidSid(owner) ||
      (!allowOwnerRepair && !EqualSid(owner, expectedOwner))) {
    throw Error("updater artifact owner is not the dedicated updater account");
  }
  const std::wstring sddl =
      updaterArtifactSddl(profile, expectedOwner, directory);
  return {std::move(object), sddl, directory};
}

enum class UpdaterRequestCaller {
  Worker,
  Coordinator,
  Interactive,
};

bool runningInUpdaterServiceContext();
bool runningInServiceContext(const wchar_t* serviceName);
bool elevatedAdministratorContext();
void requireUpdaterOrElevatedInstallerContext(const char* operation);
void requireUpdaterServiceContext(const char* operation);
UpdaterRequestCaller requireUpdaterRequestServiceContext();
std::wstring randomStage(const std::wstring& destination);

Handle openExactUpdaterParent(
    const std::wstring& path,
    const UpdaterArtifactRoots& roots) {
  const std::wstring parent = fullPath(parentPath(fullPath(path)));
  Handle directory(CreateFileW(
      parent.c_str(),
      GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
      FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
      nullptr,
      OPEN_EXISTING,
      FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
      nullptr));
  if (!directory) failWin("CreateFileW(updater artifact parent)");
  FILE_ATTRIBUTE_TAG_INFO attributes{};
  if (!GetFileInformationByHandleEx(
          directory.get(), FileAttributeTagInfo, &attributes,
          sizeof(attributes)) ||
      !(attributes.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) ||
      (attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT)) {
    throw Error("updater artifact parent is not a non-reparse directory");
  }
  const std::wstring resolved = finalDosPath(
      directory.get(),
      "GetFinalPathNameByHandleW(updater artifact parent)");
  if (!equalOrdinalIgnoreCase(resolved, parent)) {
    throw Error("updater artifact parent resolved through an untrusted path");
  }

  PSECURITY_DESCRIPTOR raw = nullptr;
  PSID owner = nullptr;
  DWORD code = GetSecurityInfo(
      directory.get(), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION,
      &owner, nullptr, nullptr, nullptr, &raw);
  if (code != ERROR_SUCCESS) failWin("GetSecurityInfo(updater parent)", code);
  Local<SECURITY_DESCRIPTOR> descriptor(
      static_cast<SECURITY_DESCRIPTOR*>(raw));
  if (!owner || !IsValidSid(owner)) {
    throw Error("updater artifact parent owner is invalid");
  }
  const auto interactiveValue =
      processEnvironmentValue(L"ROOST_INTERACTIVE_SID");
  const auto callerSid = currentSid();
  const std::wstring interactive =
      interactiveValue && !interactiveValue->empty()
      ? *interactiveValue
      : sidText(const_cast<std::uint8_t*>(callerSid.data()));
  const std::wstring ownerAccount = accountNameForSid(owner);
  const wchar_t* directoryProfile =
      equalOrdinalIgnoreCase(parent, roots.service) ? L"service-root" :
      equalOrdinalIgnoreCase(parent, roots.updater) ? L"updater-state" :
      equalOrdinalIgnoreCase(parent, roots.requests) ? L"update-inbox" :
      equalOrdinalIgnoreCase(parent, roots.bin) ? L"stable-bin" :
      equalOrdinalIgnoreCase(parent, roots.versions) ? L"versions-root" :
      nullptr;
  if (directoryProfile) {
    requireExactFileSecurity(
        directory.get(),
        directoryProtectionSddl(
            directoryProfile, ownerAccount, interactive));
    return directory;
  }
  auto updaterOwner = serviceSidForAccount(kUpdaterServiceAccount);
  for (const wchar_t* profile :
       {L"private", L"control", L"release"}) {
    try {
      requireExactFileSecurity(
          directory.get(),
          updaterArtifactSddl(profile, updaterOwner.data(), true));
      return directory;
    } catch (const Error&) {
    }
  }
  throw Error("updater artifact parent does not have an exact trusted profile");
}

Handle openUpdaterRequestParent(
    const std::wstring& path,
    const UpdaterArtifactRoots& roots,
    std::optional<UpdaterRequestCaller> caller = std::nullopt) {
  const std::wstring parent = fullPath(parentPath(fullPath(path)));
  const bool serviceInbox = equalOrdinalIgnoreCase(parent, roots.requests);
  const bool localInbox = equalOrdinalIgnoreCase(parent, roots.localRequests);
  const bool allowed = caller
      ? (*caller == UpdaterRequestCaller::Interactive
          ? localInbox
          : serviceInbox)
      : serviceInbox || localInbox;
  if (!allowed) {
    throw Error("updater request is not a direct authorized inbox child");
  }
  Handle directory(CreateFileW(
      parent.c_str(),
      FILE_ADD_FILE | FILE_TRAVERSE | SYNCHRONIZE,
      FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
      nullptr,
      OPEN_EXISTING,
      FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
      nullptr));
  if (!directory) failWin("CreateFileW(updater request parent)");
  FILE_ATTRIBUTE_TAG_INFO attributes{};
  if (!GetFileInformationByHandleEx(
          directory.get(), FileAttributeTagInfo, &attributes,
          sizeof(attributes)) ||
      !(attributes.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) ||
      (attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) ||
      !equalOrdinalIgnoreCase(
          finalDosPath(
              directory.get(),
              "GetFinalPathNameByHandleW(updater request parent)"),
          parent)) {
    throw Error("updater request parent is not its trusted inbox");
  }
  return directory;
}

void protectUpdaterArtifact(const std::vector<std::wstring>& args) {
  expect(args, 2, "protect-updater-artifact <path> <profile>");
  requireUpdaterOrElevatedInstallerContext("protect-updater-artifact");
  OpenUpdaterArtifact object = openUpdaterArtifact(
      args[0], args[1], READ_CONTROL | WRITE_DAC | WRITE_OWNER,
      FILE_SHARE_READ, true);
  setAndVerifyFileSecurity(object.handle.get(), object.sddl);
  emit("{\"protected\":true}");
}

void prepareUpdaterArtifact(const std::vector<std::wstring>& args) {
  expect(args, 2, "prepare-updater-artifact <path> <private|control>");
  requireUpdaterOrElevatedInstallerContext("prepare-updater-artifact");
  if (args[1] != L"private" && args[1] != L"control") {
    throw Error("prepared updater artifact profile is not allowlisted");
  }
  const UpdaterArtifactRoots roots = updaterArtifactRoots();
  const std::wstring path = fullPath(args[0]);
  validateUpdaterArtifactPath(path, args[1], roots);
  Handle parent = openExactUpdaterParent(path, roots);
  auto owner = serviceSidForAccount(kUpdaterServiceAccount);
  const std::wstring sddl =
      updaterArtifactSddl(args[1], owner.data(), false);
  PSECURITY_DESCRIPTOR raw = nullptr;
  if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
          sddl.c_str(), SDDL_REVISION_1, &raw, nullptr)) {
    failWin(
        "ConvertStringSecurityDescriptorToSecurityDescriptorW(prepare)");
  }
  Local<SECURITY_DESCRIPTOR> descriptor(
      static_cast<SECURITY_DESCRIPTOR*>(raw));
  SECURITY_ATTRIBUTES security{};
  security.nLength = sizeof(security);
  security.lpSecurityDescriptor = descriptor.get();
  Handle file(CreateFileW(
      path.c_str(),
      GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC | WRITE_OWNER,
      FILE_SHARE_READ,
      &security,
      CREATE_NEW,
      FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT |
          FILE_FLAG_WRITE_THROUGH,
      nullptr));
  bool created = true;
  if (!file) {
    const DWORD code = GetLastError();
    if (code != ERROR_FILE_EXISTS && code != ERROR_ALREADY_EXISTS) {
      failWin("CreateFileW(prepare updater artifact)", code);
    }
    created = false;
    OpenUpdaterArtifact existing = openUpdaterArtifact(
        path, args[1], GENERIC_READ | READ_CONTROL);
    if (existing.directory) {
      throw Error("prepared updater artifact is a directory");
    }
    requireExactFileSecurity(existing.handle.get(), existing.sddl);
  } else {
    FILE_ATTRIBUTE_TAG_INFO attributes{};
    BY_HANDLE_FILE_INFORMATION information{};
    if (!GetFileInformationByHandleEx(
            file.get(), FileAttributeTagInfo, &attributes,
            sizeof(attributes)) ||
        !GetFileInformationByHandle(file.get(), &information) ||
        (attributes.FileAttributes &
         (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)) ||
        information.nNumberOfLinks != 1) {
      throw Error("prepared updater artifact is not a unique regular file");
    }
    const std::wstring finalPath = finalDosPath(
        file.get(),
        "GetFinalPathNameByHandleW(prepared updater artifact)");
    if (!equalOrdinalIgnoreCase(finalPath, path)) {
      throw Error("prepared updater artifact resolved to a different path");
    }
    validateUpdaterArtifactPath(finalPath, args[1], roots);
    requireExactFileSecurity(file.get(), sddl);
    if (!FlushFileBuffers(file.get())) {
      failWin("FlushFileBuffers(prepared updater artifact)");
    }
  }
  emit(std::string("{\"prepared\":true,\"created\":") +
      (created ? "true}" : "false}"));
}

bool updaterRequestName(const std::wstring& name) {
  static constexpr std::wstring_view update = L"update-";
  static constexpr std::wstring_view relocation = L"relocation-";
  static constexpr std::wstring_view suffix = L".json";
  const std::size_t prefix =
      name.rfind(update, 0) == 0 ? update.size() :
      name.rfind(relocation, 0) == 0 ? relocation.size() : 0;
  if (!prefix || name.size() != prefix + 64 + suffix.size() ||
      name.compare(name.size() - suffix.size(), suffix.size(), suffix) != 0) {
    return false;
  }
  for (std::size_t index = prefix; index < prefix + 64; ++index) {
    const wchar_t ch = name[index];
    if (!((ch >= L'0' && ch <= L'9') ||
          (ch >= L'a' && ch <= L'f'))) {
      return false;
    }
  }
  return true;
}

void createUpdaterRequest(const std::vector<std::wstring>& args) {
  expect(args, 1, "create-updater-request <path>");
  const UpdaterRequestCaller caller = requireUpdaterRequestServiceContext();
  const std::vector<std::uint8_t> contents = framedInput(32U * 1024U);
  if (contents.empty()) throw Error("updater request must not be empty");
  const UpdaterArtifactRoots roots = updaterArtifactRoots();
  const std::wstring path = fullPath(args[0]);
  const std::wstring requestName = baseName(path);
  const bool callerMatchesName =
      caller == UpdaterRequestCaller::Worker
      ? requestName.rfind(L"update-", 0) == 0 ||
          requestName.rfind(L"relocation-", 0) == 0
      : caller == UpdaterRequestCaller::Coordinator
      ? requestName.rfind(L"relocation-", 0) == 0
      : requestName.rfind(L"update-", 0) == 0;
  const std::wstring parentPathValue = parentPath(path);
  const bool callerMatchesParent =
      caller == UpdaterRequestCaller::Interactive
      ? equalOrdinalIgnoreCase(parentPathValue, roots.localRequests)
      : equalOrdinalIgnoreCase(parentPathValue, roots.requests);
  if (!callerMatchesParent || !updaterRequestName(requestName) ||
      !callerMatchesName) {
    throw Error("updater request path does not match its service ancestry");
  }
  Handle parent = openUpdaterRequestParent(path, roots, caller);
  auto owner = currentSid();
  auto writer =
      caller == UpdaterRequestCaller::Worker
      ? serviceSidForName(kWorkerServiceName)
      : caller == UpdaterRequestCaller::Coordinator
      ? serviceSidForName(kCoordinatorServiceName)
      : currentSid();
  const auto updater = serviceSidForName(kUpdaterServiceName);
  std::wstring initial = L"O:" + sidText(owner.data()) + L"D:P";
  appendDirectoryAllow(initial, false, L"FA", L"SY");
  appendDirectoryAllow(initial, false, L"FA", L"BA");
  appendDirectoryAllow(initial, false, L"RC", L"S-1-3-4");
  appendDirectoryAllow(
      initial, false, L"FA",
      sidText(const_cast<std::uint8_t*>(writer.data())));
  appendDirectoryAllow(
      initial, false, L"GR",
      sidText(const_cast<std::uint8_t*>(updater.data())));
  PSECURITY_DESCRIPTOR raw = nullptr;
  if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
          initial.c_str(), SDDL_REVISION_1, &raw, nullptr)) {
    failWin(
        "ConvertStringSecurityDescriptorToSecurityDescriptorW(request)");
  }
  Local<SECURITY_DESCRIPTOR> descriptor(
      static_cast<SECURITY_DESCRIPTOR*>(raw));
  SECURITY_ATTRIBUTES security{};
  security.nLength = sizeof(security);
  security.lpSecurityDescriptor = descriptor.get();
  Handle file(CreateFileW(
      path.c_str(),
      GENERIC_WRITE | READ_CONTROL | WRITE_DAC | WRITE_OWNER,
      FILE_SHARE_READ,
      &security,
      CREATE_NEW,
      FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT |
          FILE_FLAG_WRITE_THROUGH,
      nullptr));
  if (!file) {
    const DWORD code = GetLastError();
    if (code != ERROR_FILE_EXISTS && code != ERROR_ALREADY_EXISTS) {
      failWin("CreateFileW(updater request)", code);
    }
    OpenUpdaterArtifact existing =
        openUpdaterArtifact(path, L"private", READ_CONTROL, FILE_SHARE_READ);
    if (existing.directory) {
      throw Error("existing updater request is not a file");
    }
    requireExactFileSecurity(existing.handle.get(), existing.sddl);
    emit("{\"created\":false}");
    return;
  }
  BY_HANDLE_FILE_INFORMATION information{};
  FILE_ATTRIBUTE_TAG_INFO attributes{};
  if (!GetFileInformationByHandle(file.get(), &information) ||
      !GetFileInformationByHandleEx(
          file.get(), FileAttributeTagInfo, &attributes,
          sizeof(attributes)) ||
      information.nNumberOfLinks != 1 ||
      (attributes.FileAttributes &
       (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)) ||
      !equalOrdinalIgnoreCase(
          finalDosPath(
              file.get(), "GetFinalPathNameByHandleW(updater request)"),
          path)) {
    throw Error("updater request path is not a unique trusted file");
  }
  writeAll(file.get(), contents.data(), contents.size());
  if (!FlushFileBuffers(file.get())) {
    failWin("FlushFileBuffers(updater request)");
  }
  setAndVerifyFileSecurity(
      file.get(), updaterArtifactSddl(L"private", owner.data(), false));
  if (!FlushFileBuffers(parent.get())) {
    failWin("FlushFileBuffers(updater request parent)");
  }
  emit("{\"created\":true}");
}
void consumeUpdaterRequest(const std::vector<std::wstring>& args) {
  expect(args, 1, "consume-updater-request <path>");
  requireUpdaterServiceContext("consume-updater-request");
  const UpdaterArtifactRoots roots = updaterArtifactRoots();
  const std::wstring path = fullPath(args[0]);
  const std::wstring requestParent = parentPath(path);
  if ((!equalOrdinalIgnoreCase(requestParent, roots.requests) &&
       !equalOrdinalIgnoreCase(requestParent, roots.localRequests)) ||
      !updaterRequestName(baseName(path))) {
    throw Error("consumed updater request path is not allowlisted");
  }
  Handle parent = openUpdaterRequestParent(path, roots);
  OpenUpdaterArtifact request = openUpdaterArtifact(
      path, L"private", DELETE | READ_CONTROL, 0);
  if (request.directory) {
    throw Error("consumed updater request is not a file");
  }
  requireExactFileSecurity(request.handle.get(), request.sddl);
  FILE_DISPOSITION_INFO disposition{};
  disposition.DeleteFile = TRUE;
  if (!SetFileInformationByHandle(
          request.handle.get(), FileDispositionInfo,
          &disposition, sizeof(disposition))) {
    failWin("SetFileInformationByHandle(updater request)");
  }
  request.handle.reset();
  if (!FlushFileBuffers(parent.get())) {
    failWin("FlushFileBuffers(consumed updater request parent)");
  }
  emit("{\"consumed\":true}");
}


std::string base64(const std::vector<std::uint8_t>& bytes) {
  if (bytes.empty()) return {};
  DWORD chars = 0;
  if (!CryptBinaryToStringA(
          bytes.data(), static_cast<DWORD>(bytes.size()),
          CRYPT_STRING_BASE64 | CRYPT_STRING_NOCRLF, nullptr, &chars)) {
    failWin("CryptBinaryToStringA(size)");
  }
  std::string value(chars, '\0');
  if (!CryptBinaryToStringA(
          bytes.data(), static_cast<DWORD>(bytes.size()),
          CRYPT_STRING_BASE64 | CRYPT_STRING_NOCRLF,
          value.data(), &chars)) {
    failWin("CryptBinaryToStringA");
  }
  value.resize(chars);
  if (!value.empty() && value.back() == '\0') value.pop_back();
  return value;
}

std::vector<std::uint8_t> readUpdaterArtifactContents(
    HANDLE file,
    std::uint32_t maximum) {
  LARGE_INTEGER size{};
  if (!GetFileSizeEx(file, &size) ||
      size.QuadPart < 0 ||
      static_cast<std::uint64_t>(size.QuadPart) > maximum) {
    throw Error("updater artifact exceeds its read limit");
  }
  std::vector<std::uint8_t> contents(
      static_cast<std::size_t>(size.QuadPart));
  std::size_t offset = 0;
  while (offset < contents.size()) {
    DWORD read = 0;
    const DWORD chunk = static_cast<DWORD>(
        std::min<std::size_t>(contents.size() - offset, 1U << 20));
    if (!ReadFile(
            file, contents.data() + offset, chunk, &read, nullptr)) {
      failWin("ReadFile(updater artifact)");
    }
    if (!read) throw Error("updater artifact read was truncated");
    offset += read;
  }
  return contents;
}
bool updaterStatusArtifact(
    const std::wstring& path,
    const UpdaterArtifactRoots& roots) {
  if (!equalOrdinalIgnoreCase(parentPath(path), roots.updater)) return false;
  const std::wstring name = lower(baseName(path));
  constexpr std::wstring_view prefix = L"status-";
  constexpr std::wstring_view suffix = L".json";
  if (name.size() != prefix.size() + 64 + suffix.size() ||
      name.compare(0, prefix.size(), prefix) != 0 ||
      name.compare(name.size() - suffix.size(), suffix.size(), suffix) != 0) {
    return false;
  }
  return std::all_of(
      name.begin() + static_cast<std::ptrdiff_t>(prefix.size()),
      name.end() - static_cast<std::ptrdiff_t>(suffix.size()),
      [](wchar_t ch) {
        return (ch >= L'0' && ch <= L'9') ||
            (ch >= L'a' && ch <= L'f');
      });
}

void readUpdaterArtifact(const std::vector<std::wstring>& args) {
  expect(args, 3, "read-updater-artifact <path> <profile> <max-bytes>");
  const UpdaterArtifactRoots roots = updaterArtifactRoots();
  const std::wstring path = fullPath(args[0]);
  const std::wstring relocationDir =
      fullPath(roots.updater + L"\\relocation");
  const bool statusReadable =
      args[1] == L"status" && updaterStatusArtifact(path, roots);
  const bool workerReadable = args[1] == L"control" &&
      (equalOrdinalIgnoreCase(path, fullPath(roots.updater + L"\\relocation-worker-v1.json")) ||
       equalOrdinalIgnoreCase(path, fullPath(roots.updater + L"\\relocation-coordinator-v1.json")) ||
       equalOrdinalIgnoreCase(path, fullPath(relocationDir + L"\\worker.json")));
  const bool coordinatorReadable = args[1] == L"control" &&
      equalOrdinalIgnoreCase(path, fullPath(relocationDir + L"\\coordinator.json"));
  if (!(statusReadable ||
        (workerReadable && runningInServiceContext(kWorkerServiceName)) ||
        (coordinatorReadable && runningInServiceContext(kCoordinatorServiceName)))) {
    requireUpdaterOrElevatedInstallerContext("read-updater-artifact");
  }
  const std::uint32_t maximum =
      uint32Arg(args[2], "maximum artifact bytes");
  if (maximum > kMaxFrame) {
    throw Error("maximum artifact bytes is too large");
  }
  if (statusReadable &&
      GetFileAttributesW(path.c_str()) == INVALID_FILE_ATTRIBUTES) {
    const DWORD code = GetLastError();
    if (code == ERROR_FILE_NOT_FOUND || code == ERROR_PATH_NOT_FOUND) {
      emit("{\"bytesBase64\":\"\"}");
      return;
    }
    failWin("GetFileAttributesW(updater status)", code);
  }
  OpenUpdaterArtifact object = openUpdaterArtifact(
      path, args[1], GENERIC_READ | READ_CONTROL);
  if (object.directory) {
    throw Error("updater artifact reader requires a file");
  }
  requireExactFileSecurity(object.handle.get(), object.sddl);
  const std::vector<std::uint8_t> contents =
      readUpdaterArtifactContents(object.handle.get(), maximum);
  requireExactFileSecurity(object.handle.get(), object.sddl);
  emit("{\"bytesBase64\":" + json(base64(contents)) + "}");
}

bool allowlistedUpdaterReplacement(
    const std::wstring& path,
    const std::wstring& profile,
    const UpdaterArtifactRoots& roots) {
  if (profile == L"current") {
    return equalOrdinalIgnoreCase(
        path, fullPath(roots.service + L"\\current.json"));
  }
  if (profile == L"stable-shawl") {
    return equalOrdinalIgnoreCase(
        path, fullPath(roots.bin + L"\\shawl.exe"));
  }
  if (profile == L"stable-launcher") {
    return equalOrdinalIgnoreCase(
        path, fullPath(roots.bin + L"\\roost.exe"));
  }
  if (profile == L"status") {
    return updaterStatusArtifact(path, roots);
  }
  if (profile != L"control") return false;
  if (equalOrdinalIgnoreCase(
          path,
          fullPath(roots.service + L"\\service-definitions.json")) ||
      equalOrdinalIgnoreCase(
          path,
          fullPath(roots.service + L"\\update-v2.json")) ||
      equalOrdinalIgnoreCase(
          path,
          fullPath(roots.service + L"\\update-v1.json"))) {
    return true;
  }
  const std::wstring relocationDir =
      fullPath(roots.updater + L"\\relocation");
  if (equalOrdinalIgnoreCase(parentPath(path), relocationDir)) {
    const std::wstring role = lower(baseName(path));
    return role == L"worker.json" || role == L"coordinator.json";
  }
  if (!equalOrdinalIgnoreCase(parentPath(path), roots.updater)) {
    return false;
  }
  const std::wstring name = lower(baseName(path));
  return name == L"relocation-worker-v1.json" ||
      name == L"relocation-coordinator-v1.json";
}

class UpdaterStageGuard final {
 public:
  explicit UpdaterStageGuard(std::wstring path) : path_(std::move(path)) {}
  ~UpdaterStageGuard() {
    if (active_) DeleteFileW(path_.c_str());
  }
  void release() { active_ = false; }
 private:
  std::wstring path_;
  bool active_ = true;
};

void replaceUpdaterArtifact(const std::vector<std::wstring>& args) {
  expect(args, 2, "replace-updater-artifact <path> <profile>");
  requireUpdaterServiceContext("replace-updater-artifact");
  const std::vector<std::uint8_t> contents = framedInput(kMaxFrame);
  if (contents.empty()) {
    throw Error("updater artifact replacement must not be empty");
  }
  const UpdaterArtifactRoots roots = updaterArtifactRoots();
  const std::wstring destination = fullPath(args[0]);
  validateUpdaterArtifactPath(destination, args[1], roots);
  if (!allowlistedUpdaterReplacement(
          destination, args[1], roots)) {
    throw Error("updater artifact replacement destination is not allowlisted");
  }
  Handle parent = openExactUpdaterParent(destination, roots);

  std::optional<OpenUpdaterArtifact> existing;
  Handle probe(CreateFileW(
      destination.c_str(),
      GENERIC_READ | READ_CONTROL,
      FILE_SHARE_READ | FILE_SHARE_DELETE,
      nullptr,
      OPEN_EXISTING,
      FILE_FLAG_OPEN_REPARSE_POINT,
      nullptr));
  if (probe) {
    probe.reset();
    existing.emplace(openUpdaterArtifact(
        destination,
        args[1],
        GENERIC_READ | READ_CONTROL,
        FILE_SHARE_READ | FILE_SHARE_DELETE));
    if (existing->directory) {
      throw Error("updater artifact replacement destination is a directory");
    }
    requireExactFileSecurity(
        existing->handle.get(), existing->sddl);
  } else {
    const DWORD code = GetLastError();
    if (code != ERROR_FILE_NOT_FOUND && code != ERROR_PATH_NOT_FOUND) {
      failWin("CreateFileW(updater replacement destination)", code);
    }
  }

  const std::wstring stage = randomStage(destination);
  UpdaterStageGuard stageGuard(stage);
  auto owner = serviceSidForAccount(kUpdaterServiceAccount);
  const std::wstring stageSddl =
      updaterArtifactSddl(args[1], owner.data(), false);
  PSECURITY_DESCRIPTOR raw = nullptr;
  if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
          stageSddl.c_str(), SDDL_REVISION_1, &raw, nullptr)) {
    failWin(
        "ConvertStringSecurityDescriptorToSecurityDescriptorW(replacement)");
  }
  Local<SECURITY_DESCRIPTOR> descriptor(
      static_cast<SECURITY_DESCRIPTOR*>(raw));
  SECURITY_ATTRIBUTES security{};
  security.nLength = sizeof(security);
  security.lpSecurityDescriptor = descriptor.get();
  Handle staged(CreateFileW(
      stage.c_str(),
      GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC |
          WRITE_OWNER,
      FILE_SHARE_READ | FILE_SHARE_DELETE,
      &security,
      CREATE_NEW,
      FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT |
          FILE_FLAG_WRITE_THROUGH,
      nullptr));
  if (!staged) failWin("CreateFileW(updater replacement stage)");
  FILE_ATTRIBUTE_TAG_INFO attributes{};
  BY_HANDLE_FILE_INFORMATION information{};
  if (!GetFileInformationByHandleEx(
          staged.get(), FileAttributeTagInfo, &attributes,
          sizeof(attributes)) ||
      !GetFileInformationByHandle(staged.get(), &information) ||
      (attributes.FileAttributes &
       (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)) ||
      information.nNumberOfLinks != 1 ||
      !equalOrdinalIgnoreCase(
          finalDosPath(
              staged.get(),
              "GetFinalPathNameByHandleW(updater replacement stage)"),
          stage)) {
    throw Error("updater replacement stage is not a unique trusted file");
  }
  writeAll(staged.get(), contents.data(), contents.size());
  if (!FlushFileBuffers(staged.get())) {
    failWin("FlushFileBuffers(updater replacement stage)");
  }
  requireExactFileSecurity(staged.get(), stageSddl);
  LARGE_INTEGER beginning{};
  if (!SetFilePointerEx(
          staged.get(), beginning, nullptr, FILE_BEGIN)) {
    failWin("SetFilePointerEx(updater replacement stage)");
  }
  if (readUpdaterArtifactContents(staged.get(), kMaxFrame) != contents) {
    throw Error("updater replacement stage bytes did not verify");
  }
  requireExactFileSecurity(staged.get(), stageSddl);
  staged.reset();

  if (existing) {
    if (!ReplaceFileW(
            destination.c_str(), stage.c_str(), nullptr,
            REPLACEFILE_WRITE_THROUGH, nullptr, nullptr)) {
      failWin("ReplaceFileW(updater artifact)");
    }
  } else if (!MoveFileExW(
                 stage.c_str(), destination.c_str(),
                 MOVEFILE_WRITE_THROUGH)) {
    failWin("MoveFileExW(updater artifact)");
  }
  stageGuard.release();
  existing.reset();

  OpenUpdaterArtifact replaced = openUpdaterArtifact(
      destination, args[1], GENERIC_READ | READ_CONTROL);
  if (replaced.directory) {
    throw Error("replaced updater artifact is a directory");
  }
  requireExactFileSecurity(replaced.handle.get(), replaced.sddl);
  if (readUpdaterArtifactContents(replaced.handle.get(), kMaxFrame) !=
      contents) {
    throw Error("replaced updater artifact bytes did not verify");
  }
  requireExactFileSecurity(replaced.handle.get(), replaced.sddl);
  if (!FlushFileBuffers(parent.get())) {
    failWin("FlushFileBuffers(updater replacement parent)");
  }
  emit("{\"replaced\":true,\"profile\":" + json(args[1]) +
      ",\"bytes\":" + std::to_string(contents.size()) + "}");
}

void removeUpdaterArtifact(const std::vector<std::wstring>& args) {
  expect(args, 2, "remove-updater-artifact <path> <profile>");
  requireUpdaterServiceContext("remove-updater-artifact");
  const UpdaterArtifactRoots roots = updaterArtifactRoots();
  const std::wstring destination = fullPath(args[0]);
  validateUpdaterArtifactPath(destination, args[1], roots);
  if (!allowlistedUpdaterReplacement(destination, args[1], roots)) {
    throw Error("updater artifact removal destination is not allowlisted");
  }
  Handle parent = openExactUpdaterParent(destination, roots);
  try {
    OpenUpdaterArtifact artifact = openUpdaterArtifact(
        destination, args[1], DELETE | READ_CONTROL, 0);
    if (artifact.directory) {
      throw Error("updater artifact removal destination is a directory");
    }
    requireExactFileSecurity(artifact.handle.get(), artifact.sddl);
    FILE_DISPOSITION_INFO disposition{};
    disposition.DeleteFile = TRUE;
    if (!SetFileInformationByHandle(
            artifact.handle.get(), FileDispositionInfo,
            &disposition, sizeof(disposition))) {
      failWin("SetFileInformationByHandle(updater artifact)");
    }
    artifact.handle.reset();
  } catch (const Error&) {
    const DWORD attributes = GetFileAttributesW(destination.c_str());
    if (attributes != INVALID_FILE_ATTRIBUTES) throw;
    const DWORD code = GetLastError();
    if (code != ERROR_FILE_NOT_FOUND && code != ERROR_PATH_NOT_FOUND) throw;
  }
  if (!FlushFileBuffers(parent.get())) {
    failWin("FlushFileBuffers(updater removal parent)");
  }
  emit("{\"removed\":true,\"profile\":" + json(args[1]) + "}");
}




void protectDirectoryTree(const std::vector<std::wstring>& args) {
  expect(
      args,
      4,
      "protect-directory-tree <path> <profile> <base-account> <interactive-sid>");
  const std::wstring& profile = args[1];
  if (profile != L"service-home" &&
      profile != L"keeper-state" &&
      profile != L"worker-state" &&
      profile != L"coordinator-state" &&
      profile != L"updater-state") {
    throw Error("recursive protection profile is not allowlisted");
  }
  const std::wstring root = fullPath(args[0]);
  ensureSafeParent(parentPath(root));
  const std::wstring sddl =
      directoryProtectionSddl(profile, args[2], args[3]);
  std::vector<std::wstring> pending{root};
  std::size_t protectedObjects = 0;
  std::size_t protectedDirectories = 0;
  std::size_t protectedFiles = 0;
  std::wstring trustedRoot;
  DWORD trustedVolume = 0;
  while (!pending.empty()) {
    const std::wstring path = std::move(pending.back());
    pending.pop_back();
    Handle object(CreateFileW(
        path.c_str(),
        READ_CONTROL | WRITE_DAC | WRITE_OWNER,
        FILE_SHARE_READ,
        nullptr,
        OPEN_EXISTING,
        FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
        nullptr));
    if (!object) failWin("CreateFileW(role-state tree)");
    FILE_ATTRIBUTE_TAG_INFO attributes{};
    if (!GetFileInformationByHandleEx(
            object.get(), FileAttributeTagInfo, &attributes, sizeof(attributes))) {
      failWin("GetFileInformationByHandleEx(role-state tree)");
    }
    if (attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) {
      throw Error("role-state tree contains a reparse point");
    }
    const bool directory =
        (attributes.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0;
    if (directory) ++protectedDirectories;
    else ++protectedFiles;
    BY_HANDLE_FILE_INFORMATION information{};
    if (!GetFileInformationByHandle(object.get(), &information)) {
      failWin("GetFileInformationByHandle(role-state tree)");
    }
    const std::wstring finalPath =
        finalDosPath(object.get(), "GetFinalPathNameByHandleW(role-state tree)");
    if (protectedObjects == 0) {
      if (!directory) throw Error("role-state tree root is not a directory");
      trustedRoot = finalPath;
      trustedVolume = information.dwVolumeSerialNumber;
    } else if (
        information.dwVolumeSerialNumber != trustedVolume ||
        !pathAtOrBelow(trustedRoot, finalPath)) {
      throw Error("role-state tree object escaped its trusted root");
    }
    if (!directory && information.nNumberOfLinks != 1) {
      throw Error("role-state tree contains a multiply-linked file");
    }
    setAndVerifyFileSecurity(object.get(), sddl);
    if (!directory) object.reset();
    if (++protectedObjects > kMaxZipEntries) {
      throw Error("role-state tree contains too many objects");
    }
    if (!directory) continue;

    WIN32_FIND_DATAW data{};
    FindHandle search(FindFirstFileW((path + L"\\*").c_str(), &data));
    if (!search) failWin("FindFirstFileW(role-state tree)");
    do {
      if (!std::wcscmp(data.cFileName, L".") ||
          !std::wcscmp(data.cFileName, L"..")) {
        continue;
      }
      if (data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) {
        throw Error("role-state tree contains a reparse point");
      }
      pending.push_back(path + L"\\" + data.cFileName);
    } while (FindNextFileW(search.get(), &data));
    if (GetLastError() != ERROR_NO_MORE_FILES) {
      failWin("FindNextFileW(role-state tree)");
    }
    object.reset();
  }
  emit("{\"protected\":true,\"profile\":" + json(profile) +
      ",\"objects\":" + std::to_string(protectedObjects) +
      ",\"directories\":" + std::to_string(protectedDirectories) +
      ",\"files\":" + std::to_string(protectedFiles) + "}");
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

enum class StableArtifactKind {
  Shawl,
  Launcher,
};

struct UpdaterArtifactIdentity {
  std::array<std::uint8_t, 32> sha256{};
  std::uint64_t size = 0;
};

struct UpdaterArtifactProof {
  UpdaterArtifactIdentity identity;
  std::wstring sddl;
};

std::wstring checkedUpdaterArtifactAbsolutePath(
    const std::wstring& path,
    const char* label) {
  if (path.size() < 3 || !std::iswalpha(path[0]) || path[1] != L':' ||
      (path[2] != L'\\' && path[2] != L'/')) {
    throw Error(std::string(label) + " must be an absolute local Windows path");
  }
  return fullPath(path);
}

std::uint64_t updaterArtifactSizeArg(
    const std::wstring& text,
    const char* label) {
  if (text.empty()) throw Error(std::string("invalid ") + label);
  std::uint64_t value = 0;
  for (wchar_t ch : text) {
    if (ch < L'0' || ch > L'9') {
      throw Error(std::string("invalid ") + label);
    }
    const std::uint64_t digit = static_cast<unsigned>(ch - L'0');
    if (value > (std::numeric_limits<std::uint64_t>::max() - digit) / 10) {
      throw Error(std::string("invalid ") + label);
    }
    value = value * 10 + digit;
  }
  if (!value || value > 9007199254740991ULL) {
    throw Error(std::string("invalid ") + label);
  }
  return value;
}

std::optional<UpdaterArtifactIdentity> updaterArtifactExpected(
    const std::vector<std::wstring>& args,
    std::size_t first) {
  if (args.size() == first) return std::nullopt;
  if (args.size() != first + 2) {
    throw Error("updater artifact identity requires SHA-256 and size");
  }
  return UpdaterArtifactIdentity{
      parseSha256(toUtf8(args[first])),
      updaterArtifactSizeArg(args[first + 1], "updater artifact size"),
  };
}

bool lowerHex64(const std::wstring& value) {
  if (value.size() != 64) return false;
  return std::all_of(value.begin(), value.end(), [](wchar_t ch) {
    return (ch >= L'0' && ch <= L'9') ||
        (ch >= L'a' && ch <= L'f');
  });
}

std::optional<StableArtifactKind> stableArtifactProfileKind(
    const std::wstring& profile) {
  if (profile == L"stable-shawl") return StableArtifactKind::Shawl;
  if (profile == L"stable-launcher") return StableArtifactKind::Launcher;
  return std::nullopt;
}

std::optional<StableArtifactKind> releaseStableArtifactKind(
    const std::wstring& path,
    const UpdaterArtifactRoots& roots) {
  const std::wstring versionDir = parentPath(path);
  if (!equalOrdinalIgnoreCase(parentPath(versionDir), roots.versions)) {
    return std::nullopt;
  }
  const std::wstring version = baseName(versionDir);
  if (version.empty() || version == L"." || version == L".." ||
      !std::all_of(version.begin(), version.end(), [](wchar_t ch) {
        return std::iswalnum(ch) || ch == L'.' || ch == L'-' ||
            ch == L'_' || ch == L'+';
      })) {
    return std::nullopt;
  }
  const std::wstring name = lower(baseName(path));
  if (name == L"shawl.exe") return StableArtifactKind::Shawl;
  if (name == L"roost-win-helper.exe") {
    return StableArtifactKind::Launcher;
  }
  return std::nullopt;
}

bool releaseInspectionArtifactPath(
    const std::wstring& path,
    const UpdaterArtifactRoots& roots) {
  if (equalOrdinalIgnoreCase(
          path, fullPath(roots.bin + L"\\install-root.txt")) ||
      equalOrdinalIgnoreCase(
          path, fullPath(roots.bin + L"\\publisher.sha256"))) {
    return true;
  }
  std::wstring prefix = roots.versions;
  if (prefix.empty() ||
      (prefix.back() != L'\\' && prefix.back() != L'/')) {
    prefix.push_back(L'\\');
  }
  if (path.size() <= prefix.size() ||
      !equalOrdinalIgnoreCase(path.substr(0, prefix.size()), prefix)) {
    return false;
  }
  const std::wstring relative = path.substr(prefix.size());
  const std::size_t slash = relative.find_first_of(L"\\/");
  if (slash == std::wstring::npos || slash == 0 ||
      slash + 1 >= relative.size()) {
    return false;
  }
  const std::wstring version = relative.substr(0, slash);
  if (!std::all_of(version.begin(), version.end(), [](wchar_t ch) {
        return std::iswalnum(ch) || ch == L'.' || ch == L'-' ||
            ch == L'_' || ch == L'+';
      })) {
    return false;
  }
  const std::wstring versionDir = fullPath(prefix + version);
  return pathAtOrBelow(versionDir, path);
}

std::optional<StableArtifactKind> privateStableBackupKind(
    const std::wstring& path,
    const UpdaterArtifactRoots& roots) {
  const std::wstring stableArtifacts = parentPath(path);
  const std::wstring transaction = parentPath(stableArtifacts);
  const std::wstring updates = parentPath(transaction);
  if (!equalOrdinalIgnoreCase(
          updates, fullPath(roots.updater + L"\\updates")) ||
      !equalOrdinalIgnoreCase(
          baseName(stableArtifacts), L"stable-artifacts") ||
      !lowerHex64(baseName(transaction))) {
    return std::nullopt;
  }
  const std::wstring name = lower(baseName(path));
  if (name == L"shawl.bak") return StableArtifactKind::Shawl;
  if (name == L"launcher.bak") return StableArtifactKind::Launcher;
  return std::nullopt;
}

void requireInspectUpdaterArtifactPath(
    const std::wstring& path,
    const std::wstring& profile,
    const UpdaterArtifactRoots& roots) {
  if (profile == L"current") {
    if (!equalOrdinalIgnoreCase(
            path, fullPath(roots.service + L"\\current.json"))) {
      throw Error("current artifact inspection path is not allowlisted");
    }
  } else if (profile == L"release") {
    if (!releaseInspectionArtifactPath(path, roots)) {
      throw Error("release artifact inspection path is not allowlisted");
    }
  } else if (profile == L"private") {
    if (!privateStableBackupKind(path, roots)) {
      throw Error("private artifact inspection path is not allowlisted");
    }
  } else if (const auto kind = stableArtifactProfileKind(profile)) {
    const std::wstring expected = *kind == StableArtifactKind::Shawl
        ? fullPath(roots.bin + L"\\shawl.exe")
        : fullPath(roots.bin + L"\\roost.exe");
    if (!equalOrdinalIgnoreCase(path, expected)) {
      throw Error("stable artifact inspection path is not allowlisted");
    }
  } else {
    throw Error("artifact inspection profile is not allowlisted");
  }
  validateUpdaterArtifactPath(path, profile, roots);
}

UpdaterArtifactProof hashHeldUpdaterArtifact(
    HANDLE file,
    const std::wstring& expectedSddl,
    const char* label) {
  requireExactFileSecurity(file, expectedSddl);
  const std::wstring before = objectSecuritySddl(file);
  if (before.empty() || before.size() > 64U * 1024U) {
    throw Error("updater artifact security proof is too large");
  }
  LARGE_INTEGER length{};
  if (!GetFileSizeEx(file, &length) || length.QuadPart < 0 ||
      static_cast<std::uint64_t>(length.QuadPart) > 9007199254740991ULL) {
    throw Error(std::string(label) + " has an invalid proof size");
  }
  LARGE_INTEGER beginning{};
  if (!SetFilePointerEx(file, beginning, nullptr, FILE_BEGIN)) {
    failWin("SetFilePointerEx(updater artifact proof)");
  }
  Sha256 hash;
  std::array<std::uint8_t, 64 * 1024> buffer{};
  std::uint64_t total = 0;
  for (;;) {
    DWORD got = 0;
    if (!ReadFile(
            file, buffer.data(), static_cast<DWORD>(buffer.size()),
            &got, nullptr)) {
      failWin("ReadFile(updater artifact proof)");
    }
    if (!got) break;
    if (total > static_cast<std::uint64_t>(length.QuadPart) - got) {
      throw Error(std::string(label) + " grew while being proved");
    }
    total += got;
    hash.update(buffer.data(), got);
  }
  if (total != static_cast<std::uint64_t>(length.QuadPart)) {
    throw Error(std::string(label) + " changed size while being proved");
  }
  LARGE_INTEGER finalLength{};
  if (!GetFileSizeEx(file, &finalLength) ||
      finalLength.QuadPart != length.QuadPart) {
    throw Error(std::string(label) + " changed while being proved");
  }
  requireExactFileSecurity(file, expectedSddl);
  const std::wstring after = objectSecuritySddl(file);
  if (after != before) {
    throw Error(std::string(label) + " security changed while being proved");
  }
  return {{hash.finish(), total}, before};
}

void enforceUpdaterArtifactExpected(
    const UpdaterArtifactProof& proof,
    const std::optional<UpdaterArtifactIdentity>& expected) {
  if (expected &&
      (proof.identity.size != expected->size ||
       proof.identity.sha256 != expected->sha256)) {
    throw Error("updater artifact identity differs from the expected proof");
  }
}

UpdaterArtifactIdentity streamHeldArtifactToAtomicDestination(
    HANDLE source,
    const std::wstring& destinationPath,
    HANDLE destinationParent,
    const std::wstring& destinationSddl,
    const char* label,
    bool allowReplace = true,
    const std::optional<std::wstring>& alternateExistingSddl = std::nullopt,
    const std::optional<std::wstring>& expectedSourceSddl = std::nullopt,
    const std::optional<UpdaterArtifactIdentity>& expectedSourceIdentity =
        std::nullopt) {
  const std::wstring expectedParent =
      fullPath(parentPath(destinationPath));
  const auto requireHeldParentIdentity = [&] {
    FILE_ATTRIBUTE_TAG_INFO attributes{};
    if (!GetFileInformationByHandleEx(
            destinationParent, FileAttributeTagInfo, &attributes,
            sizeof(attributes)) ||
        !(attributes.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) ||
        (attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) ||
        !equalOrdinalIgnoreCase(
            finalDosPath(
                destinationParent,
                "GetFinalPathNameByHandleW(held destination parent)"),
            expectedParent)) {
      throw Error(
          std::string(label) +
          " destination parent identity changed during mutation");
    }
    ensureSafeParent(expectedParent);
  };
  requireHeldParentIdentity();
  LARGE_INTEGER sourceLength{};
  if (!GetFileSizeEx(source, &sourceLength) ||
      sourceLength.QuadPart < 0 ||
      static_cast<std::uint64_t>(sourceLength.QuadPart) >
          9007199254740991ULL) {
    throw Error(std::string(label) + " source has an invalid size");
  }
  LARGE_INTEGER beginning{};
  if (!SetFilePointerEx(source, beginning, nullptr, FILE_BEGIN)) {
    failWin("SetFilePointerEx(held artifact source)");
  }

  const std::wstring stage = randomStage(destinationPath);
  UpdaterStageGuard stageGuard(stage);
  PSECURITY_DESCRIPTOR raw = nullptr;
  if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
          destinationSddl.c_str(), SDDL_REVISION_1, &raw, nullptr)) {
    failWin(
        "ConvertStringSecurityDescriptorToSecurityDescriptorW(held stage)");
  }
  Local<SECURITY_DESCRIPTOR> descriptor(
      static_cast<SECURITY_DESCRIPTOR*>(raw));
  SECURITY_ATTRIBUTES security{};
  security.nLength = sizeof(security);
  security.lpSecurityDescriptor = descriptor.get();
  requireHeldParentIdentity();
  Handle staged(CreateFileW(
      stage.c_str(),
      GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC | WRITE_OWNER,
      0,
      &security,
      CREATE_NEW,
      FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT |
          FILE_FLAG_SEQUENTIAL_SCAN | FILE_FLAG_WRITE_THROUGH,
      nullptr));
  if (!staged) failWin("CreateFileW(held artifact stage)");
  FILE_ATTRIBUTE_TAG_INFO stageAttributes{};
  BY_HANDLE_FILE_INFORMATION stageInformation{};
  if (!GetFileInformationByHandleEx(
          staged.get(), FileAttributeTagInfo, &stageAttributes,
          sizeof(stageAttributes)) ||
      !GetFileInformationByHandle(staged.get(), &stageInformation) ||
      (stageAttributes.FileAttributes &
       (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)) ||
      stageInformation.nNumberOfLinks != 1 ||
      !equalOrdinalIgnoreCase(
          finalDosPath(
              staged.get(),
              "GetFinalPathNameByHandleW(held artifact stage)"),
          stage)) {
    throw Error(std::string(label) + " stage is not a unique exact file");
  }
  requireExactFileSecurity(staged.get(), destinationSddl);
  requireHeldParentIdentity();

  Sha256 sourceHash;
  std::array<std::uint8_t, 64 * 1024> buffer{};
  std::uint64_t copied = 0;
  for (;;) {
    DWORD got = 0;
    if (!ReadFile(
            source, buffer.data(), static_cast<DWORD>(buffer.size()),
            &got, nullptr)) {
      failWin("ReadFile(held artifact source)");
    }
    if (!got) break;
    if (copied > static_cast<std::uint64_t>(sourceLength.QuadPart) - got) {
      throw Error(std::string(label) + " source grew while streaming");
    }
    copied += got;
    sourceHash.update(buffer.data(), got);
    writeAll(staged.get(), buffer.data(), got);
  }
  if (copied != static_cast<std::uint64_t>(sourceLength.QuadPart)) {
    throw Error(std::string(label) + " source changed while streaming");
  }
  const UpdaterArtifactIdentity sourceIdentity{sourceHash.finish(), copied};
  if (!FlushFileBuffers(staged.get())) {
    failWin("FlushFileBuffers(held artifact stage)");
  }
  const UpdaterArtifactProof stagedProof = hashHeldUpdaterArtifact(
      staged.get(), destinationSddl, "held artifact stage");
  if (stagedProof.identity.size != sourceIdentity.size ||
      stagedProof.identity.sha256 != sourceIdentity.sha256) {
    throw Error(std::string(label) + " stage differs from its source");
  }
  if (expectedSourceIdentity &&
      (sourceIdentity.size != expectedSourceIdentity->size ||
       sourceIdentity.sha256 != expectedSourceIdentity->sha256)) {
    throw Error(std::string(label) + " source differs from its prior proof");
  }
  if (expectedSourceSddl) {
    requireExactFileSecurity(source, *expectedSourceSddl);
    if (objectSecuritySddl(source) != *expectedSourceSddl) {
      throw Error(std::string(label) + " source security changed before commit");
    }
  }
  staged.reset();

  requireHeldParentIdentity();
  bool destinationExists = false;
  Handle destinationProbe(CreateFileW(
      destinationPath.c_str(),
      GENERIC_READ | READ_CONTROL,
      FILE_SHARE_READ | FILE_SHARE_DELETE,
      nullptr,
      OPEN_EXISTING,
      FILE_FLAG_OPEN_REPARSE_POINT,
      nullptr));
  if (destinationProbe) {
    FILE_ATTRIBUTE_TAG_INFO attributes{};
    BY_HANDLE_FILE_INFORMATION information{};
    if (!GetFileInformationByHandleEx(
            destinationProbe.get(), FileAttributeTagInfo, &attributes,
            sizeof(attributes)) ||
        !GetFileInformationByHandle(destinationProbe.get(), &information) ||
        (attributes.FileAttributes &
         (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)) ||
        information.nNumberOfLinks != 1 ||
        !equalOrdinalIgnoreCase(
            finalDosPath(
                destinationProbe.get(),
                "GetFinalPathNameByHandleW(held artifact destination)"),
            destinationPath)) {
      throw Error(
          std::string(label) +
          " destination is not a unique exact regular file");
    }
    if (!fileSecurityMatchesExact(
            destinationProbe.get(), destinationSddl) &&
        (!alternateExistingSddl ||
         !fileSecurityMatchesExact(
             destinationProbe.get(), *alternateExistingSddl))) {
      throw Error(
          std::string(label) +
          " destination security is not an admitted exact descriptor");
    }
    if (!allowReplace) {
      throw Error(std::string(label) + " destination already exists");
    }
    destinationExists = true;
    destinationProbe.reset();
  } else {
    const DWORD code = GetLastError();
    if (code != ERROR_FILE_NOT_FOUND && code != ERROR_PATH_NOT_FOUND) {
      failWin("CreateFileW(held artifact destination)", code);
    }
  }

  requireHeldParentIdentity();
  if (destinationExists) {
    if (alternateExistingSddl) {
      if (!MoveFileExW(
              stage.c_str(), destinationPath.c_str(),
              MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)) {
        failWin("MoveFileExW(held artifact ACL transition)");
      }
    } else if (!ReplaceFileW(
                   destinationPath.c_str(), stage.c_str(), nullptr,
                   REPLACEFILE_WRITE_THROUGH, nullptr, nullptr)) {
      failWin("ReplaceFileW(held artifact)");
    }
  } else if (!MoveFileExW(
                 stage.c_str(), destinationPath.c_str(),
                 MOVEFILE_WRITE_THROUGH)) {
    failWin("MoveFileExW(held artifact)");
  }
  requireHeldParentIdentity();
  stageGuard.release();
  if (!FlushFileBuffers(destinationParent)) {
    failWin("FlushFileBuffers(held artifact parent)");
  }

  requireHeldParentIdentity();
  Handle finalFile(CreateFileW(
      destinationPath.c_str(),
      GENERIC_READ | READ_CONTROL,
      FILE_SHARE_READ,
      nullptr,
      OPEN_EXISTING,
      FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_SEQUENTIAL_SCAN,
      nullptr));
  if (!finalFile) failWin("CreateFileW(final held artifact)");
  FILE_ATTRIBUTE_TAG_INFO finalAttributes{};
  BY_HANDLE_FILE_INFORMATION finalInformation{};
  if (!GetFileInformationByHandleEx(
          finalFile.get(), FileAttributeTagInfo, &finalAttributes,
          sizeof(finalAttributes)) ||
      !GetFileInformationByHandle(finalFile.get(), &finalInformation) ||
      (finalAttributes.FileAttributes &
       (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)) ||
      finalInformation.nNumberOfLinks != 1 ||
      !equalOrdinalIgnoreCase(
          finalDosPath(
              finalFile.get(),
              "GetFinalPathNameByHandleW(final held artifact)"),
          destinationPath)) {
    throw Error(std::string(label) + " final file is not a unique exact file");
  }
  requireHeldParentIdentity();
  const UpdaterArtifactProof finalProof = hashHeldUpdaterArtifact(
      finalFile.get(), destinationSddl, "final held artifact");
  if (finalProof.identity.size != sourceIdentity.size ||
      finalProof.identity.sha256 != sourceIdentity.sha256) {
    throw Error(std::string(label) + " final file differs from its source");
  }
  return sourceIdentity;
}

class HeldUpdaterArtifactParents final {
 public:
  HANDLE get() const {
    if (handles_.empty()) throw Error("updater artifact parent is not held");
    return handles_.back().get();
  }
  void add(Handle handle) { handles_.emplace_back(std::move(handle)); }
 private:
  std::vector<Handle> handles_;
};

HeldUpdaterArtifactParents openOrCreatePrivateBackupParent(
    const std::wstring& destination,
    const UpdaterArtifactRoots& roots) {
  if (!privateStableBackupKind(destination, roots)) {
    throw Error("private backup destination path is not allowlisted");
  }
  HeldUpdaterArtifactParents held;
  held.add(openExactUpdaterParent(
      fullPath(roots.updater + L"\\.roost-parent-proof"), roots));

  auto owner = serviceSidForAccount(kUpdaterServiceAccount);
  const std::wstring directorySddl =
      updaterArtifactSddl(L"private", owner.data(), true);
  PSECURITY_DESCRIPTOR raw = nullptr;
  if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
          directorySddl.c_str(), SDDL_REVISION_1, &raw, nullptr)) {
    failWin(
        "ConvertStringSecurityDescriptorToSecurityDescriptorW(backup parent)");
  }
  Local<SECURITY_DESCRIPTOR> descriptor(
      static_cast<SECURITY_DESCRIPTOR*>(raw));
  SECURITY_ATTRIBUTES security{};
  security.nLength = sizeof(security);
  security.lpSecurityDescriptor = descriptor.get();

  const std::wstring destinationParent = parentPath(destination);
  std::size_t cursor = roots.updater.size();
  while (cursor < destinationParent.size()) {
    if (destinationParent[cursor] != L'\\' &&
        destinationParent[cursor] != L'/') {
      throw Error("private backup parent escaped its updater root");
    }
    const std::size_t slash =
        destinationParent.find_first_of(L"\\/", cursor + 1);
    const std::wstring current = slash == std::wstring::npos
        ? destinationParent
        : destinationParent.substr(0, slash);
    bool created = false;
    if (CreateDirectoryW(current.c_str(), &security)) {
      created = true;
    } else {
      const DWORD code = GetLastError();
      if (code != ERROR_ALREADY_EXISTS) {
        failWin("CreateDirectoryW(private backup parent)", code);
      }
    }
    Handle directory(CreateFileW(
        current.c_str(),
        GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        nullptr,
        OPEN_EXISTING,
        FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
        nullptr));
    if (!directory) failWin("CreateFileW(private backup parent)");
    FILE_ATTRIBUTE_TAG_INFO attributes{};
    if (!GetFileInformationByHandleEx(
            directory.get(), FileAttributeTagInfo, &attributes,
            sizeof(attributes)) ||
        !(attributes.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) ||
        (attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) ||
        !equalOrdinalIgnoreCase(
            finalDosPath(
                directory.get(),
                "GetFinalPathNameByHandleW(private backup parent)"),
            current)) {
      throw Error("private backup parent is not an exact non-reparse directory");
    }
    requireExactFileSecurity(directory.get(), directorySddl);
    if (created && !FlushFileBuffers(held.get())) {
      failWin("FlushFileBuffers(created backup parent)");
    }
    held.add(std::move(directory));
    if (slash == std::wstring::npos) break;
    cursor = slash;
  }
  validateUpdaterArtifactPath(destination, L"private", roots);
  return held;
}

void inspectUpdaterArtifact(const std::vector<std::wstring>& args) {
  if (args.size() != 2 && args.size() != 4) {
    throw Error(
        "usage: inspect-updater-artifact <path> <profile> "
        "[<expected-sha256> <expected-size>]");
  }
  requireUpdaterOrElevatedInstallerContext("inspect-updater-artifact");
  const std::optional<UpdaterArtifactIdentity> expected =
      updaterArtifactExpected(args, 2);
  const UpdaterArtifactRoots roots = updaterArtifactRoots();
  const std::wstring path =
      checkedUpdaterArtifactAbsolutePath(args[0], "artifact path");
  requireInspectUpdaterArtifactPath(path, args[1], roots);
  OpenUpdaterArtifact object = openUpdaterArtifact(
      path, args[1], GENERIC_READ | READ_CONTROL);
  if (object.directory) {
    throw Error("inspected updater artifact is not a file");
  }
  const UpdaterArtifactProof proof = hashHeldUpdaterArtifact(
      object.handle.get(), object.sddl, "inspected updater artifact");
  enforceUpdaterArtifactExpected(proof, expected);
  emit("{\"profile\":" + json(args[1]) +
      ",\"sha256\":" + json(hexHash(proof.identity.sha256)) +
      ",\"size\":" + std::to_string(proof.identity.size) +
      ",\"sddl\":" + json(proof.sddl) + "}");
}

void copyUpdaterArtifact(const std::vector<std::wstring>& args) {
  if (args.size() != 4 && args.size() != 6) {
    throw Error(
        "usage: copy-updater-artifact <source> <destination> "
        "<source-profile> <destination-profile> "
        "[<expected-sha256> <expected-size>]");
  }
  requireUpdaterOrElevatedInstallerContext("copy-updater-artifact");
  const std::optional<UpdaterArtifactIdentity> expected =
      updaterArtifactExpected(args, 4);
  const UpdaterArtifactRoots roots = updaterArtifactRoots();
  const std::wstring sourcePath =
      checkedUpdaterArtifactAbsolutePath(args[0], "artifact source");
  const std::wstring destinationPath =
      checkedUpdaterArtifactAbsolutePath(args[1], "artifact destination");
  const std::wstring& sourceProfile = args[2];
  const std::wstring& destinationProfile = args[3];

  const std::optional<StableArtifactKind> stableSource =
      stableArtifactProfileKind(sourceProfile);
  const std::optional<StableArtifactKind> stableDestination =
      stableArtifactProfileKind(destinationProfile);
  const std::optional<StableArtifactKind> releaseSource =
      sourceProfile == L"release"
      ? releaseStableArtifactKind(sourcePath, roots)
      : std::nullopt;
  const std::optional<StableArtifactKind> privateSource =
      sourceProfile == L"private"
      ? privateStableBackupKind(sourcePath, roots)
      : std::nullopt;
  const std::optional<StableArtifactKind> privateDestination =
      destinationProfile == L"private"
      ? privateStableBackupKind(destinationPath, roots)
      : std::nullopt;
  const bool snapshot =
      stableSource && privateDestination &&
      *stableSource == *privateDestination && !expected;
  const bool promotion =
      releaseSource && stableDestination &&
      *releaseSource == *stableDestination && expected;
  const bool rollback =
      privateSource && stableDestination &&
      *privateSource == *stableDestination && expected;
  if (!snapshot && !promotion && !rollback) {
    throw Error("updater artifact copy matrix is not allowlisted");
  }
  validateUpdaterArtifactPath(sourcePath, sourceProfile, roots);
  if (destinationProfile != L"private") {
    validateUpdaterArtifactPath(
        destinationPath, destinationProfile, roots);
  }

  OpenUpdaterArtifact source = openUpdaterArtifact(
      sourcePath, sourceProfile, GENERIC_READ | READ_CONTROL);
  if (source.directory) {
    throw Error("updater artifact copy source is not a file");
  }
  const UpdaterArtifactProof sourceProof = hashHeldUpdaterArtifact(
      source.handle.get(), source.sddl, "updater artifact copy source");
  enforceUpdaterArtifactExpected(sourceProof, expected);

  HeldUpdaterArtifactParents parents;
  if (destinationProfile == L"private") {
    parents = openOrCreatePrivateBackupParent(destinationPath, roots);
  } else {
    parents.add(openExactUpdaterParent(destinationPath, roots));
  }

  std::optional<OpenUpdaterArtifact> existing;
  Handle probe(CreateFileW(
      destinationPath.c_str(), GENERIC_READ | READ_CONTROL,
      FILE_SHARE_READ | FILE_SHARE_DELETE, nullptr, OPEN_EXISTING,
      FILE_FLAG_OPEN_REPARSE_POINT, nullptr));
  if (probe) {
    probe.reset();
    existing.emplace(openUpdaterArtifact(
        destinationPath, destinationProfile,
        GENERIC_READ | READ_CONTROL,
        FILE_SHARE_READ | FILE_SHARE_DELETE));
    if (existing->directory) {
      throw Error("updater artifact copy destination is a directory");
    }
    requireExactFileSecurity(existing->handle.get(), existing->sddl);
  } else {
    const DWORD code = GetLastError();
    if (code != ERROR_FILE_NOT_FOUND && code != ERROR_PATH_NOT_FOUND) {
      failWin("CreateFileW(updater artifact copy destination)", code);
    }
  }
  if (snapshot && existing) {
    const UpdaterArtifactProof backupProof = hashHeldUpdaterArtifact(
        existing->handle.get(), existing->sddl,
        "existing stable artifact backup");
    if (backupProof.identity.size != sourceProof.identity.size ||
        backupProof.identity.sha256 != sourceProof.identity.sha256) {
      throw Error(
          "existing stable artifact backup differs from the held source");
    }
    requireExactFileSecurity(source.handle.get(), source.sddl);
    if (objectSecuritySddl(source.handle.get()) != sourceProof.sddl) {
      throw Error("stable artifact source security changed before retry proof");
    }
    emit("{\"sourceProfile\":" + json(sourceProfile) +
        ",\"destinationProfile\":" + json(destinationProfile) +
        ",\"sha256\":" + json(hexHash(sourceProof.identity.sha256)) +
        ",\"size\":" + std::to_string(sourceProof.identity.size) +
        ",\"sddl\":" + json(sourceProof.sddl) + "}");
    return;
  }

  auto owner = serviceSidForAccount(kUpdaterServiceAccount);
  const std::wstring destinationSddl =
      updaterArtifactSddl(destinationProfile, owner.data(), false);
  const UpdaterArtifactIdentity streamed =
      streamHeldArtifactToAtomicDestination(
          source.handle.get(), destinationPath, parents.get(),
          destinationSddl, "updater artifact copy", !snapshot, std::nullopt,
          sourceProof.sddl, sourceProof.identity);
  existing.reset();
  if (streamed.size != sourceProof.identity.size ||
      streamed.sha256 != sourceProof.identity.sha256) {
    throw Error("streamed updater artifact differs from its source proof");
  }

  OpenUpdaterArtifact destination = openUpdaterArtifact(
      destinationPath, destinationProfile,
      GENERIC_READ | READ_CONTROL);
  if (destination.directory) {
    throw Error("copied updater artifact destination is not a file");
  }
  const UpdaterArtifactProof destinationProof = hashHeldUpdaterArtifact(
      destination.handle.get(), destination.sddl,
      "copied updater artifact destination");
  if (destinationProof.identity.size != sourceProof.identity.size ||
      destinationProof.identity.sha256 != sourceProof.identity.sha256) {
    throw Error("copied updater artifact differs from its source");
  }
  requireExactFileSecurity(source.handle.get(), source.sddl);
  if (objectSecuritySddl(source.handle.get()) != sourceProof.sddl) {
    throw Error("updater artifact source security changed before proof");
  }
  emit("{\"sourceProfile\":" + json(sourceProfile) +
      ",\"destinationProfile\":" + json(destinationProfile) +
      ",\"sha256\":" + json(hexHash(sourceProof.identity.sha256)) +
      ",\"size\":" + std::to_string(sourceProof.identity.size) +
      ",\"sddl\":" + json(sourceProof.sddl) + "}");
}


struct ExtractionSecurity {
  std::vector<std::uint8_t> owner;
};

std::wstring extractedObjectSddl(
    bool directory,
    const ExtractionSecurity& security) {
  return updaterArtifactSddl(
      L"release",
      const_cast<std::uint8_t*>(security.owner.data()),
      directory);
}

Handle protectExtractedObject(
    const std::wstring& path,
    bool directory,
    const ExtractionSecurity& security,
    DWORD additionalAccess = 0) {
  Handle object(CreateFileW(
      path.c_str(),
      READ_CONTROL | WRITE_DAC | WRITE_OWNER | additionalAccess,
      FILE_SHARE_READ,
      nullptr,
      OPEN_EXISTING,
      FILE_FLAG_OPEN_REPARSE_POINT |
          (directory ? FILE_FLAG_BACKUP_SEMANTICS : FILE_FLAG_SEQUENTIAL_SCAN),
      nullptr));
  if (!object) failWin("CreateFileW(extracted object)");
  FILE_ATTRIBUTE_TAG_INFO tag{};
  if (!GetFileInformationByHandleEx(
      object.get(), FileAttributeTagInfo, &tag, sizeof(tag))) {
    failWin("GetFileInformationByHandleEx(extracted object)");
  }
  const bool actualDirectory = (tag.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0;
  if ((tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) ||
      actualDirectory != directory) {
    throw Error("extracted object type changed during validation");
  }
  setAndVerifyFileSecurity(object.get(), extractedObjectSddl(directory, security));
  return object;
}

std::array<std::uint8_t, 32> hashAndFlush(
    const std::wstring& path,
    std::uint64_t expectedSize,
    const ExtractionSecurity& security) {
  Handle file = protectExtractedObject(
      path, false, security, GENERIC_READ | GENERIC_WRITE);
  BY_HANDLE_FILE_INFORMATION info{};
  if (!GetFileInformationByHandle(file.get(), &info)) failWin("GetFileInformationByHandle");
  if (info.nNumberOfLinks != 1) throw Error("hard-linked extracted file is forbidden");
  Sha256 hash;
  std::array<std::uint8_t, 64 * 1024> buffer{};
  std::uint64_t total = 0;
  for (;;) {
    DWORD got = 0;
    if (!ReadFile(file.get(), buffer.data(), static_cast<DWORD>(buffer.size()), &got, nullptr)) {
      failWin("ReadFile(extracted)");
    }
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
    bool final, const ExtractionSecurity* security,
    std::set<std::wstring>& actual, std::uint64_t& total) {
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
      if (final) {
        if (!security) throw Error("final extraction scan is missing its security profile");
        Handle protectedDirectory = protectExtractedObject(
            root + L"\\" + childRelative, true, *security);
      }
      scanStage(root, childRelative, manifest, final, security, actual, total);
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
      if (!security) throw Error("final extraction scan is missing its security profile");
      if (size != expectedFile.size) throw Error("extracted file size differs from manifest");
      std::wstring path = root + L"\\" + childRelative;
      if (hashAndFlush(path, expectedFile.size, *security) != expectedFile.sha256) {
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

std::wstring moduleFilePath() {
  std::wstring path(32768, L'\0');
  const DWORD length = GetModuleFileNameW(
      nullptr, path.data(), static_cast<DWORD>(path.size()));
  if (!length || length >= path.size()) failWin("GetModuleFileNameW");
  path.resize(length);
  return path;
}

struct LockedRegularContents {
  Handle handle;
  std::vector<std::uint8_t> bytes;
};

LockedRegularContents readLockedRegularFile(
    const std::wstring& path,
    std::uint64_t maximum,
    DWORD sharing) {
  Handle file(CreateFileW(
      path.c_str(),
      GENERIC_READ,
      sharing,
      nullptr,
      OPEN_EXISTING,
      FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_SEQUENTIAL_SCAN,
      nullptr));
  if (!file) failWin("CreateFileW(launcher metadata)");
  FILE_ATTRIBUTE_TAG_INFO attributes{};
  if (!GetFileInformationByHandleEx(
      file.get(), FileAttributeTagInfo, &attributes, sizeof(attributes))) {
    failWin("GetFileInformationByHandleEx(launcher metadata)");
  }
  if (attributes.FileAttributes &
      (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)) {
    throw Error("launcher metadata is not a regular non-reparse file");
  }
  LARGE_INTEGER length{};
  if (!GetFileSizeEx(file.get(), &length)) failWin("GetFileSizeEx(launcher metadata)");
  if (length.QuadPart < 0 || static_cast<std::uint64_t>(length.QuadPart) > maximum ||
      static_cast<std::uint64_t>(length.QuadPart) >
          static_cast<std::uint64_t>(std::numeric_limits<std::size_t>::max())) {
    throw Error("launcher metadata exceeds its size limit");
  }
  std::vector<std::uint8_t> bytes(static_cast<std::size_t>(length.QuadPart));
  std::size_t offset = 0;
  while (offset < bytes.size()) {
    DWORD got = 0;
    const DWORD want = static_cast<DWORD>(
        std::min<std::size_t>(bytes.size() - offset, 1U << 20));
    if (!ReadFile(file.get(), bytes.data() + offset, want, &got, nullptr)) {
      failWin("ReadFile(launcher metadata)");
    }
    if (!got) throw Error("launcher metadata changed while being read");
    offset += got;
  }
  return {std::move(file), std::move(bytes)};
}

std::string trimmedFileText(const std::vector<std::uint8_t>& bytes) {
  if (std::find(bytes.begin(), bytes.end(), std::uint8_t{0}) != bytes.end()) {
    throw Error("launcher metadata contains NUL");
  }
  std::string text(reinterpret_cast<const char*>(bytes.data()), bytes.size());
  while (!text.empty() &&
      (text.back() == ' ' || text.back() == '\t' || text.back() == '\r' ||
       text.back() == '\n')) {
    text.pop_back();
  }
  std::size_t start = 0;
  while (start < text.size() &&
      (text[start] == ' ' || text[start] == '\t' || text[start] == '\r' ||
       text[start] == '\n')) {
    ++start;
  }
  text.erase(0, start);
  if (text.empty()) throw Error("launcher metadata is empty");
  return text;
}

std::wstring checkedLocalAbsolutePath(std::string_view utf8, const char* label) {
  std::wstring path = fromUtf8(utf8);
  if (path.find(L'\0') != std::wstring::npos ||
      path.size() < 3 ||
      !std::iswalpha(path[0]) ||
      path[1] != L':' ||
      (path[2] != L'\\' && path[2] != L'/')) {
    throw Error(std::string(label) + " must be an absolute local Windows path");
  }
  return fullPath(path);
}

struct LockedActiveFile {
  Handle handle;
  std::uint64_t size;
};

LockedActiveFile lockActiveFile(const std::wstring& path) {
  Handle file(CreateFileW(
      path.c_str(),
      GENERIC_READ,
      FILE_SHARE_READ,
      nullptr,
      OPEN_EXISTING,
      FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_SEQUENTIAL_SCAN,
      nullptr));
  if (!file) failWin("CreateFileW(active Roost file)");
  FILE_ATTRIBUTE_TAG_INFO attributes{};
  if (!GetFileInformationByHandleEx(
      file.get(), FileAttributeTagInfo, &attributes, sizeof(attributes))) {
    failWin("GetFileInformationByHandleEx(active Roost file)");
  }
  if (attributes.FileAttributes &
      (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)) {
    throw Error("active Roost file is not a regular non-reparse file");
  }
  LARGE_INTEGER length{};
  if (!GetFileSizeEx(file.get(), &length)) failWin("GetFileSizeEx(active Roost file)");
  if (length.QuadPart < 0) throw Error("active Roost file has an invalid size");
  return {std::move(file), static_cast<std::uint64_t>(length.QuadPart)};
}

std::array<std::uint8_t, 32> hashLockedFile(
    LockedActiveFile& file,
    std::uint64_t expectedSize) {
  if (file.size != expectedSize) {
    throw Error("active Roost file size differs from current manifest");
  }
  LARGE_INTEGER beginning{};
  if (!SetFilePointerEx(file.handle.get(), beginning, nullptr, FILE_BEGIN)) {
    failWin("SetFilePointerEx(active Roost file)");
  }
  Sha256 hash;
  std::array<std::uint8_t, 64 * 1024> buffer{};
  std::uint64_t total = 0;
  for (;;) {
    DWORD got = 0;
    if (!ReadFile(
        file.handle.get(), buffer.data(), static_cast<DWORD>(buffer.size()), &got, nullptr)) {
      failWin("ReadFile(active Roost file)");
    }
    if (!got) break;
    if (total > expectedSize - std::min<std::uint64_t>(expectedSize, got)) {
      throw Error("active Roost file exceeds its current manifest size");
    }
    total += got;
    if (total > expectedSize) {
      throw Error("active Roost file exceeds its current manifest size");
    }
    hash.update(buffer.data(), got);
  }
  if (total != expectedSize) {
    throw Error("active Roost file size differs from current manifest");
  }
  return hash.finish();
}

bool pathWithin(const std::wstring& parent, const std::wstring& child) {
  std::wstring prefix = parent;
  if (!prefix.empty() && prefix.back() != L'\\' && prefix.back() != L'/') prefix.push_back(L'\\');
  return child.size() > prefix.size() &&
      equalOrdinalIgnoreCase(prefix, child.substr(0, prefix.size()));
}

struct ExpectedActiveFile {
  std::uint64_t size = 0;
  std::array<std::uint8_t, 32> sha256{};
  bool found = false;
};

struct VerifiedCurrentRoost {
  LockedRegularContents current;
  std::wstring versionDir;
  std::wstring executablePath;
  std::wstring helperPath;
  LockedActiveFile executable;
  LockedActiveFile helper;
};

VerifiedCurrentRoost verifyCurrentRoost(
    const std::wstring& versionsRoot,
    const std::wstring& serviceDir,
    const std::string& publisher) {
  LockedRegularContents currentFile = readLockedRegularFile(
      serviceDir + L"\\current.json",
      16ULL * 1024ULL * 1024ULL,
      FILE_SHARE_READ | FILE_SHARE_DELETE);
  const JsonValue current = JsonParser(std::string_view(
      reinterpret_cast<const char*>(currentFile.bytes.data()),
      currentFile.bytes.size())).parse();
  if (current.type != JsonValue::Type::Object) {
    throw Error("current manifest must be a JSON object");
  }
  const std::uint64_t schema =
      member(current, "schemaVersion", JsonValue::Type::Number).number;
  if (schema != 1 && schema != 2) {
    throw Error("unsupported current manifest schema");
  }
  const JsonValue& version = member(current, "version", JsonValue::Type::String);
  if (version.string.empty()) throw Error("current manifest version is empty");
  const auto build = current.object.find("build");
  if (schema == 2) {
    if (build == current.object.end() ||
        build->second.type != JsonValue::Type::String ||
        build->second.string.empty()) {
      throw Error("current manifest build is missing or invalid");
    }
  } else if (build != current.object.end() &&
      (build->second.type != JsonValue::Type::String ||
       build->second.string.empty())) {
    throw Error("legacy current manifest build is invalid");
  }
  const JsonValue& manifestUrl =
      member(current, "manifestUrl", JsonValue::Type::String);
  if (manifestUrl.string.empty()) throw Error("current manifest URL is empty");
  (void)parseSha256(
      member(current, "manifestSha256", JsonValue::Type::String).string);
  const std::string currentPublisher = checkedPublisher(fromUtf8(
      member(current, "publisherSha256", JsonValue::Type::String).string));
  if (currentPublisher != publisher) {
    throw Error("current manifest publisher differs from the stable publisher pin");
  }

  const std::wstring versionDir = checkedLocalAbsolutePath(
      member(current, "versionDir", JsonValue::Type::String).string,
      "current versionDir");
  if (!pathWithin(versionsRoot, versionDir)) {
    throw Error("active Roost version escaped the configured versions directory");
  }
  ensureSafeParent(versionDir);

  const JsonValue& files = member(current, "files", JsonValue::Type::Array);
  if (files.array.empty() || files.array.size() > kMaxZipEntries) {
    throw Error("current manifest file list is empty or too large");
  }
  std::set<std::wstring> seen;
  ExpectedActiveFile expectedExecutable;
  ExpectedActiveFile expectedHelper;
  for (const JsonValue& item : files.array) {
    if (item.type != JsonValue::Type::Object || item.object.size() != 3 ||
        !item.object.contains("path") ||
        !item.object.contains("size") ||
        !item.object.contains("sha256")) {
      throw Error("current manifest file must contain exactly path, size, and sha256");
    }
    NormalPath path = normalizeArchivePath(
        member(item, "path", JsonValue::Type::String).string, false);
    if (!seen.insert(path.canonical).second) {
      throw Error("duplicate current manifest file path");
    }
    const std::uint64_t size =
        member(item, "size", JsonValue::Type::Number).number;
    if (size > kMaxZipBytes) throw Error("current manifest file is too large");
    const auto sha256 =
        parseSha256(member(item, "sha256", JsonValue::Type::String).string);
    ExpectedActiveFile* expected = nullptr;
    if (path.canonical == L"roost.exe") {
      expected = &expectedExecutable;
    } else if (path.canonical == L"roost-win-helper.exe") {
      expected = &expectedHelper;
    }
    if (expected) {
      expected->size = size;
      expected->sha256 = sha256;
      expected->found = true;
    }
  }
  if (!expectedExecutable.found || !expectedHelper.found) {
    throw Error("current manifest is missing an active Roost executable");
  }

  const std::wstring executablePath = fullPath(versionDir + L"\\roost.exe");
  const std::wstring helperPath =
      fullPath(versionDir + L"\\roost-win-helper.exe");
  LockedActiveFile executable = lockActiveFile(executablePath);
  LockedActiveFile helper = lockActiveFile(helperPath);
  if (hashLockedFile(executable, expectedExecutable.size) !=
          expectedExecutable.sha256 ||
      hashLockedFile(helper, expectedHelper.size) != expectedHelper.sha256) {
    throw Error("active Roost file SHA-256 differs from current manifest");
  }
  inspectAuthenticode(executablePath, publisher);
  inspectAuthenticode(helperPath, publisher);
  return {
      std::move(currentFile),
      versionDir,
      executablePath,
      helperPath,
      std::move(executable),
      std::move(helper),
  };
}

int launchCurrentRoost(
    const std::wstring& launcherPath,
    const std::vector<std::wstring>& arguments) {
  const std::wstring launcherDir = fullPath(parentPath(launcherPath));
  ensureSafeParent(launcherDir);
  LockedRegularContents installRootMetadata = readLockedRegularFile(
      launcherDir + L"\\install-root.txt", 32768, FILE_SHARE_READ);
  LockedRegularContents publisherMetadata = readLockedRegularFile(
      launcherDir + L"\\publisher.sha256", 1024, FILE_SHARE_READ);
  const std::wstring installRoot = checkedLocalAbsolutePath(
      trimmedFileText(installRootMetadata.bytes), "stable install root");
  const std::string publisher = checkedPublisher(fromUtf8(
      trimmedFileText(publisherMetadata.bytes)));
  if (!equalOrdinalIgnoreCase(lower(baseName(launcherDir)), L"bin") ||
      !equalOrdinalIgnoreCase(fullPath(parentPath(launcherDir)), installRoot)) {
    throw Error("stable launcher directory is not beneath its configured install root");
  }
  ensureSafeParent(installRoot);
  const std::wstring versionsRoot = fullPath(installRoot + L"\\versions");
  const std::wstring serviceDir = fullPath(installRoot + L"\\service");
  ensureSafeParent(versionsRoot);
  ensureSafeParent(serviceDir);
  VerifiedCurrentRoost active =
      verifyCurrentRoost(versionsRoot, serviceDir, publisher);

  if (!SetEnvironmentVariableW(L"ROOST_SERVICE_DIR", serviceDir.c_str()) ||
      !SetEnvironmentVariableW(L"ROOST_VERSIONS_DIR", versionsRoot.c_str()) ||
      !SetEnvironmentVariableW(L"ROOST_WIN_HELPER", active.helperPath.c_str())) {
    failWin("SetEnvironmentVariableW(Roost launcher)");
  }

  std::wstring command = quoteArg(active.executablePath);
  for (const std::wstring& argument : arguments) {
    command += L" " + quoteArg(argument);
  }
  std::vector<wchar_t> mutableCommand(command.begin(), command.end());
  mutableCommand.push_back(L'\0');
  STARTUPINFOW startup{};
  startup.cb = sizeof(startup);
  startup.dwFlags = STARTF_USESTDHANDLES;
  startup.hStdInput = GetStdHandle(STD_INPUT_HANDLE);
  startup.hStdOutput = GetStdHandle(STD_OUTPUT_HANDLE);
  startup.hStdError = GetStdHandle(STD_ERROR_HANDLE);
  PROCESS_INFORMATION process{};
  if (!CreateProcessW(
      active.executablePath.c_str(),
      mutableCommand.data(),
      nullptr,
      nullptr,
      TRUE,
      0,
      nullptr,
      nullptr,
      &startup,
      &process)) {
    failWin("CreateProcessW(active Roost)");
  }
  Handle child(process.hProcess);
  Handle childThread(process.hThread);
  const DWORD waited = WaitForSingleObject(child.get(), INFINITE);
  if (waited != WAIT_OBJECT_0) failWin("WaitForSingleObject(active Roost)");
  DWORD exitCode = 0;
  if (!GetExitCodeProcess(child.get(), &exitCode)) {
    failWin("GetExitCodeProcess(active Roost)");
  }
  return static_cast<int>(exitCode);
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
      scanStage(stage, L"", manifest, false, nullptr, actual, total);
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
  requireUpdaterOrElevatedInstallerContext("extract-zip");
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
  ExtractionSecurity security{
      serviceSidForAccount(kUpdaterServiceAccount)};

  std::wstring stage = randomStage(destination);
  if (!CreateDirectoryW(stage.c_str(), nullptr)) failWin("CreateDirectoryW(staging)");
  StageGuard guard(stage);
  {
    Handle protectedStage = protectExtractedObject(stage, true, security);
  }
  runTar(archive, stage, manifest);
  {
    Handle protectedStage = protectExtractedObject(stage, true, security);
  }
  std::set<std::wstring> actual;
  std::uint64_t total = 0;
  scanStage(stage, L"", manifest, true, &security, actual, total);
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
  return name == kKeeperServiceName || name == kWorkerServiceName ||
      name == kCoordinatorServiceName || name == kUpdaterServiceName;
}

void requireService(const std::wstring& name) {
  if (!allowedService(name)) throw Error("service name is not in the Roost V2 allowlist");
}

const wchar_t* serviceAccountFor(const std::wstring& service) {
  requireService(service);
  if (service == kKeeperServiceName) return kKeeperServiceAccount;
  if (service == kWorkerServiceName) return kWorkerServiceAccount;
  if (service == kCoordinatorServiceName) return kCoordinatorServiceAccount;
  return kUpdaterServiceAccount;
}

void resolveServiceSid(const std::vector<std::wstring>& args) {
  expect(args, 1, "resolve-service-sid <service>");
  const auto sid = serviceSidForName(args[0]);
  emit("{\"sid\":" +
      json(sidText(const_cast<std::uint8_t*>(sid.data()))) + "}");
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
  const std::wstring canonical = domain.empty() ? account : domain + L"\\" + account;
  const bool localAccount = use == SidTypeUser && equalOrdinalIgnoreCase(domain, localComputerName());
  const bool administrator = userIsAdministrator(canonical);
  emit("{\"sid\":" + json(sidText(sid.data())) +
      ",\"canonicalAccount\":" + json(canonical) +
      ",\"localAccount\":" + std::string(localAccount ? "true" : "false") +
      ",\"administrator\":" + std::string(administrator ? "true" : "false") + "}");
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
  const std::array<std::wstring, 3> requiredRights = {
      L"SeServiceLogonRight",
      L"SeDenyInteractiveLogonRight",
      L"SeDenyRemoteInteractiveLogonRight",
  };
  std::set<std::wstring> present;
  PLSA_UNICODE_STRING rights = nullptr;
  ULONG count = 0;
  status = LsaEnumerateAccountRights(policy.get(), sid.get(), &rights, &count);
  if (status == 0) {
    for (ULONG i = 0; i < count; ++i) {
      present.emplace(rights[i].Buffer, rights[i].Length / sizeof(wchar_t));
    }
    LsaFreeMemory(rights);
  } else if (static_cast<ULONG>(status) != 0xC0000034UL) {
    failLsa("LsaEnumerateAccountRights", status);
  }
  std::vector<LSA_UNICODE_STRING> missing;
  for (const std::wstring& name : requiredRights) {
    if (present.contains(name)) continue;
    LSA_UNICODE_STRING right{};
    right.Buffer = const_cast<wchar_t*>(name.data());
    right.Length = static_cast<USHORT>(name.size() * sizeof(wchar_t));
    right.MaximumLength = right.Length + sizeof(wchar_t);
    missing.push_back(right);
  }
  if (!missing.empty()) {
    status = LsaAddAccountRights(
        policy.get(), sid.get(), missing.data(), static_cast<ULONG>(missing.size()));
    if (status) failLsa("LsaAddAccountRights", status);
  }
  emit(std::string("{\"changed\":") + (missing.empty() ? "false}" : "true}"));
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

struct ServiceAccessAce {
  bool allow = false;
  bool deny = false;
  ACCESS_MASK mask = 0;
  PSID sid = nullptr;
};

bool parseServiceAccessAce(const ACE_HEADER* header, ServiceAccessAce& out) {
  const bool basic =
      header->AceType == ACCESS_ALLOWED_ACE_TYPE ||
      header->AceType == ACCESS_DENIED_ACE_TYPE ||
      header->AceType == 0x09 ||
      header->AceType == 0x0a;
  const bool object =
      header->AceType == ACCESS_ALLOWED_OBJECT_ACE_TYPE ||
      header->AceType == ACCESS_DENIED_OBJECT_ACE_TYPE ||
      header->AceType == 0x0b ||
      header->AceType == 0x0c;
  if (!basic && !object) return false;
  const auto* bytes = reinterpret_cast<const std::uint8_t*>(header);
  constexpr std::size_t maskOffset = sizeof(ACE_HEADER);
  constexpr std::size_t baseSidOffset = sizeof(ACE_HEADER) + sizeof(ACCESS_MASK);
  if (header->AceSize < baseSidOffset + 8) {
    throw Error("service DACL contains a truncated access ACE");
  }
  std::memcpy(&out.mask, bytes + maskOffset, sizeof(out.mask));
  std::size_t sidOffset = baseSidOffset;
  if (object) {
    if (header->AceSize < sidOffset + sizeof(DWORD) + 8) {
      throw Error("service DACL contains a truncated object ACE");
    }
    DWORD flags = 0;
    std::memcpy(&flags, bytes + sidOffset, sizeof(flags));
    sidOffset += sizeof(flags);
    if (flags & ACE_OBJECT_TYPE_PRESENT) sidOffset += sizeof(GUID);
    if (flags & ACE_INHERITED_OBJECT_TYPE_PRESENT) sidOffset += sizeof(GUID);
    if (sidOffset + 8 > header->AceSize) {
      throw Error("service DACL object ACE has an invalid SID offset");
    }
  }
  out.sid = const_cast<std::uint8_t*>(bytes + sidOffset);
  if (!IsValidSid(out.sid) ||
      GetLengthSid(out.sid) > header->AceSize - sidOffset) {
    throw Error("service DACL contains an invalid access ACE SID");
  }
  out.allow =
      header->AceType == ACCESS_ALLOWED_ACE_TYPE ||
      header->AceType == ACCESS_ALLOWED_OBJECT_ACE_TYPE ||
      header->AceType == 0x09 ||
      header->AceType == 0x0b;
  out.deny = !out.allow;
  return true;
}

PACL serviceDacl(std::vector<std::uint8_t>& security) {
  BOOL present = FALSE;
  BOOL defaulted = FALSE;
  PACL dacl = nullptr;
  if (!GetSecurityDescriptorDacl(
      reinterpret_cast<PSECURITY_DESCRIPTOR>(security.data()),
      &present,
      &dacl,
      &defaulted) ||
      !present || !dacl || !IsValidAcl(dacl)) {
    throw Error("service has no valid DACL");
  }
  return dacl;
}

void setServiceDacl(SC_HANDLE service, PACL dacl) {
  SECURITY_DESCRIPTOR descriptor{};
  if (!InitializeSecurityDescriptor(&descriptor, SECURITY_DESCRIPTOR_REVISION) ||
      !SetSecurityDescriptorDacl(&descriptor, TRUE, dacl, FALSE)) {
    failWin("SetSecurityDescriptorDacl");
  }
  if (!SetServiceObjectSecurity(service, DACL_SECURITY_INFORMATION, &descriptor)) {
    failWin("SetServiceObjectSecurity");
  }
}

std::string serviceSecurityJson(const std::vector<std::uint8_t>& security) {
  wchar_t* rawText = nullptr;
  if (!ConvertSecurityDescriptorToStringSecurityDescriptorW(
      reinterpret_cast<PSECURITY_DESCRIPTOR>(
          const_cast<std::uint8_t*>(security.data())),
      SDDL_REVISION_1,
      DACL_SECURITY_INFORMATION,
      &rawText,
      nullptr)) {
    failWin("ConvertSecurityDescriptorToStringSecurityDescriptorW");
  }
  Local<wchar_t> text(rawText);
  return "{\"sddl\":" + json(std::wstring(text.get())) + "}";
}

constexpr ACCESS_MASK kForbiddenServiceDaclRights =
    WRITE_DAC | WRITE_OWNER;

void verifyAppliedServiceAce(
    PACL dacl,
    PSID sid,
    ACCESS_MASK expectedMask) {
  DWORD matchingAllows = 0;
  for (DWORD index = 0; index < dacl->AceCount; ++index) {
    void* rawAce = nullptr;
    if (!GetAce(dacl, index, &rawAce)) failWin("GetAce(service DACL)");
    const auto* header = static_cast<const ACE_HEADER*>(rawAce);
    ServiceAccessAce access;
    if (!parseServiceAccessAce(header, access) ||
        (header->AceFlags & INHERITED_ACE) ||
        !EqualSid(access.sid, sid)) {
      continue;
    }
    if (access.deny) {
      throw Error("service DACL retains an explicit deny ACE for the target SID");
    }
    if ((access.mask & kForbiddenServiceDaclRights) != 0 ||
        access.mask != expectedMask ||
        header->AceFlags != 0) {
      throw Error("service DACL target ACE is broader than the approved rights");
    }
    ++matchingAllows;
  }
  if (matchingAllows != 1) {
    throw Error("service DACL target SID was not installed as one exact ACE");
  }
}

void applyServiceDacl(const std::vector<std::wstring>& args) {
  expect(args, 3, "apply-service-dacl <service> <sid> <rights>");
  requireService(args[0]);
  ACCESS_MASK rights = 0;
  if (args[2] == L"QUERY_STATUS,QUERY_CONFIG") {
    rights = SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG;
  } else if (args[2] == L"START,QUERY_STATUS,QUERY_CONFIG") {
    rights = SERVICE_START | SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG;
  } else if (args[2] == L"START,STOP,QUERY_STATUS,QUERY_CONFIG") {
    rights = SERVICE_START | SERVICE_STOP |
        SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG;
  } else if (
      args[2] ==
      L"CHANGE_CONFIG,START,STOP,QUERY_STATUS,QUERY_CONFIG") {
    rights = SERVICE_CHANGE_CONFIG | SERVICE_START | SERVICE_STOP |
        SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG;
  } else {
    throw Error("service DACL rights are not the approved allowlist");
  }
  auto sid = sidFromText(args[1], "service DACL SID");
  ServiceHandle manager = scm();
  ServiceHandle service(OpenServiceW(
      manager.get(), args[0].c_str(), READ_CONTROL | WRITE_DAC));
  if (!service) failWin("OpenServiceW");
  std::vector<std::uint8_t> security = serviceSecurity(service.get());
  PACL existing = serviceDacl(security);
  EXPLICIT_ACCESSW entry{};
  entry.grfAccessPermissions = rights;
  entry.grfAccessMode = SET_ACCESS;
  entry.grfInheritance = NO_INHERITANCE;
  entry.Trustee.TrusteeForm = TRUSTEE_IS_SID;
  entry.Trustee.TrusteeType = TRUSTEE_IS_UNKNOWN;
  entry.Trustee.ptstrName = reinterpret_cast<LPWSTR>(sid.data());
  PACL rawAcl = nullptr;
  DWORD code = SetEntriesInAclW(1, &entry, existing, &rawAcl);
  if (code != ERROR_SUCCESS) failWin("SetEntriesInAclW", code);
  Local<ACL> acl(rawAcl);
  setServiceDacl(service.get(), acl.get());

  security = serviceSecurity(service.get());
  verifyAppliedServiceAce(serviceDacl(security), sid.data(), rights);
  emit(serviceSecurityJson(security));
}

void verifyServiceSidRevoked(PACL dacl, PSID sid) {
  for (DWORD index = 0; index < dacl->AceCount; ++index) {
    void* rawAce = nullptr;
    if (!GetAce(dacl, index, &rawAce)) failWin("GetAce(service DACL)");
    const auto* header = static_cast<const ACE_HEADER*>(rawAce);
    ServiceAccessAce access;
    if (parseServiceAccessAce(header, access) &&
        !(header->AceFlags & INHERITED_ACE) &&
        EqualSid(access.sid, sid)) {
      throw Error("service DACL still contains an explicit ACE for the revoked SID");
    }
  }
}

void revokeServiceDacl(const std::vector<std::wstring>& args) {
  expect(args, 2, "revoke-service-dacl <service> <sid>");
  requireService(args[0]);
  auto sid = sidFromText(args[1], "revoked service DACL SID");
  ServiceHandle manager = scm();
  ServiceHandle service(OpenServiceW(
      manager.get(), args[0].c_str(), READ_CONTROL | WRITE_DAC));
  if (!service) failWin("OpenServiceW");
  std::vector<std::uint8_t> security = serviceSecurity(service.get());
  PACL existing = serviceDacl(security);
  std::vector<std::uint8_t> replacement(existing->AclSize);
  PACL replacementAcl = reinterpret_cast<PACL>(replacement.data());
  if (!InitializeAcl(
      replacementAcl,
      static_cast<DWORD>(replacement.size()),
      existing->AclRevision)) {
    failWin("InitializeAcl(service DACL)");
  }
  bool changed = false;
  for (DWORD index = 0; index < existing->AceCount; ++index) {
    void* rawAce = nullptr;
    if (!GetAce(existing, index, &rawAce)) failWin("GetAce(service DACL)");
    const auto* header = static_cast<const ACE_HEADER*>(rawAce);
    ServiceAccessAce access;
    const bool remove =
        parseServiceAccessAce(header, access) &&
        !(header->AceFlags & INHERITED_ACE) &&
        EqualSid(access.sid, sid.data());
    if (remove) {
      changed = true;
      continue;
    }
    if (!AddAce(
        replacementAcl,
        existing->AclRevision,
        MAXDWORD,
        rawAce,
        header->AceSize)) {
      failWin("AddAce(service DACL)");
    }
  }
  if (changed) setServiceDacl(service.get(), replacementAcl);
  security = serviceSecurity(service.get());
  verifyServiceSidRevoked(serviceDacl(security), sid.data());
  std::string result = serviceSecurityJson(security);
  result.pop_back();
  result += std::string(",\"changed\":") + (changed ? "true}" : "false}");
  emit(std::move(result));
}

DWORD queryServiceSidTypeValue(HANDLE service) {
  DWORD needed = 0;
  QueryServiceConfig2W(
      service, SERVICE_CONFIG_SERVICE_SID_INFO, nullptr, 0, &needed);
  if (GetLastError() != ERROR_INSUFFICIENT_BUFFER ||
      needed < sizeof(SERVICE_SID_INFO)) {
    failWin("QueryServiceConfig2W(SERVICE_CONFIG_SERVICE_SID_INFO)");
  }
  std::vector<std::uint8_t> buffer(needed);
  if (!QueryServiceConfig2W(
          service, SERVICE_CONFIG_SERVICE_SID_INFO, buffer.data(), needed,
          &needed)) {
    failWin("QueryServiceConfig2W(SERVICE_CONFIG_SERVICE_SID_INFO)");
  }
  return reinterpret_cast<const SERVICE_SID_INFO*>(buffer.data())
      ->dwServiceSidType;
}

const char* serviceSidTypeName(DWORD value) {
  switch (value) {
    case SERVICE_SID_TYPE_NONE: return "none";
    case SERVICE_SID_TYPE_RESTRICTED: return "restricted";
    case SERVICE_SID_TYPE_UNRESTRICTED: return "unrestricted";
    default: throw Error("SCM returned an unsupported service SID type");
  }
}

void configureServiceSid(const std::vector<std::wstring>& args) {
  expect(args, 2, "configure-service-sid <service> <none|restricted|unrestricted>");
  requireService(args[0]);
  const DWORD requested =
      args[1] == L"none" ? SERVICE_SID_TYPE_NONE :
      args[1] == L"restricted" ? SERVICE_SID_TYPE_RESTRICTED :
      args[1] == L"unrestricted" ? SERVICE_SID_TYPE_UNRESTRICTED : MAXDWORD;
  if (requested == MAXDWORD) {
    throw Error("service SID type is not allowlisted");
  }
  ServiceHandle manager = scm();
  ServiceHandle service(OpenServiceW(
      manager.get(),
      args[0].c_str(),
      SERVICE_CHANGE_CONFIG | SERVICE_QUERY_CONFIG));
  if (!service) failWin("OpenServiceW");
  SERVICE_SID_INFO desired{};
  desired.dwServiceSidType = requested;
  if (!ChangeServiceConfig2W(
      service.get(), SERVICE_CONFIG_SERVICE_SID_INFO, &desired)) {
    failWin("ChangeServiceConfig2W(SERVICE_CONFIG_SERVICE_SID_INFO)");
  }

  DWORD needed = 0;
  QueryServiceConfig2W(
      service.get(), SERVICE_CONFIG_SERVICE_SID_INFO, nullptr, 0, &needed);
  if (GetLastError() != ERROR_INSUFFICIENT_BUFFER ||
      needed < sizeof(SERVICE_SID_INFO)) {
    failWin("QueryServiceConfig2W(SERVICE_CONFIG_SERVICE_SID_INFO)");
  }
  std::vector<std::uint8_t> buffer(needed);
  if (!QueryServiceConfig2W(
      service.get(),
      SERVICE_CONFIG_SERVICE_SID_INFO,
      buffer.data(),
      needed,
      &needed)) {
    failWin("QueryServiceConfig2W(SERVICE_CONFIG_SERVICE_SID_INFO)");
  }
  const auto* actual =
      reinterpret_cast<const SERVICE_SID_INFO*>(buffer.data());
  if (actual->dwServiceSidType != requested) {
    throw Error("service SID type did not round-trip");
  }
  emit("{\"configured\":true,\"sidType\":" + json(args[1]) + "}");
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
  if (args.size() != 1 && !(args.size() == 2 && args[1] == L"basic")) {
    throw Error("usage: service-query <service> [basic]");
  }
  requireService(args[0]);
  const bool includeSecurity = args.size() == 1;
  ServiceHandle manager = scm();
  const DWORD access = SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS |
      (includeSecurity ? READ_CONTROL : 0);
  ServiceHandle service(OpenServiceW(manager.get(), args[0].c_str(), access));
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
  const char* sidType = serviceSidTypeName(queryServiceSidTypeValue(service.get()));
  const std::wstring securityDescriptor = includeSecurity ? queryServiceSddl(service.get()) : L"";
  const char* startType = config.startType == SERVICE_AUTO_START ? "automatic" :
      config.startType == SERVICE_DEMAND_START ? "manual" :
      config.startType == SERVICE_DISABLED ? "disabled" : nullptr;
  if (!startType) throw Error("unsupported service start type");
  std::string out = "{\"name\":" + json(args[0]) + ",\"state\":" + json(serviceState(status.dwCurrentState)) +
      ",\"serviceSidType\":" + json(sidType) +
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

bool tokenContainsEnabledSid(PSID sid) {
  BOOL member = FALSE;
  if (!CheckTokenMembership(nullptr, sid, &member)) {
    failWin("CheckTokenMembership");
  }
  return member != FALSE;
}

bool runningInServiceContext(const wchar_t* serviceName) {
  const auto serviceSid = serviceSidForName(serviceName);
  if (!tokenContainsEnabledSid(
          const_cast<std::uint8_t*>(serviceSid.data()))) {
    return false;
  }
  ServiceHandle manager = scm();
  SC_HANDLE raw = OpenServiceW(
      manager.get(), serviceName,
      SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS);
  if (!raw) {
    const DWORD code = GetLastError();
    if (code == ERROR_SERVICE_DOES_NOT_EXIST) return false;
    failWin("OpenServiceW(service context)", code);
  }
  ServiceHandle service(raw);
  const ServiceConfigSnapshot config = queryConfig(service.get());
  const std::vector<std::wstring> configuredArgv =
      splitCommandLine(config.image);
  if (configuredArgv.empty() ||
      lower(baseName(configuredArgv[0])) != L"shawl.exe" ||
      configuredArgv[0].size() < 3 ||
      !std::iswalpha(configuredArgv[0][0]) ||
      configuredArgv[0][1] != L':' ||
      (configuredArgv[0][2] != L'\\' &&
       configuredArgv[0][2] != L'/')) {
    throw Error("service context is not SCM-configured through exact Shawl");
  }
  const SERVICE_STATUS_PROCESS status = queryStatus(service.get());
  const std::wstring runningImage = status.dwProcessId
      ? processImage(status.dwProcessId)
      : std::wstring();
  if (runningImage.empty() ||
      !equalOrdinalIgnoreCase(
          fullPath(runningImage), fullPath(configuredArgv[0]))) {
    return false;
  }

  DWORD helperParent = 0;
  const auto parents = parentMap(&helperParent);
  if (!helperParent) return false;
  std::array<DWORD, 4> servicePids{
      servicePidIfRunning(manager.get(), kKeeperServiceName),
      servicePidIfRunning(manager.get(), kWorkerServiceName),
      servicePidIfRunning(manager.get(), kCoordinatorServiceName),
      servicePidIfRunning(manager.get(), kUpdaterServiceName)};
  bool reached = false;
  std::set<DWORD> visited;
  for (DWORD cursor = helperParent;
       cursor && visited.insert(cursor).second;) {
    if (cursor == status.dwProcessId) {
      reached = true;
      break;
    }
    for (DWORD servicePid : servicePids) {
      if (servicePid && servicePid != status.dwProcessId &&
          cursor == servicePid) {
        return false;
      }
    }
    const auto found = parents.find(cursor);
    if (found == parents.end()) break;
    cursor = found->second;
  }
  return reached;
}

bool runningInUpdaterServiceContext() {
  return runningInServiceContext(kUpdaterServiceName);
}

std::wstring trustedUpdaterInstallRoot() {
  const auto absoluteLocal = [](const std::wstring& path, const char* label) {
    if (path.size() < 3 || !std::iswalpha(path[0]) || path[1] != L':' ||
        (path[2] != L'\\' && path[2] != L'/')) {
      throw Error(std::string(label) + " must be an absolute local path");
    }
    return fullPath(path);
  };
  const std::wstring helperPath =
      absoluteLocal(moduleFilePath(), "active helper module");
  if (!equalOrdinalIgnoreCase(baseName(helperPath), L"roost-win-helper.exe")) {
    throw Error("updater artifact helper is not roost-win-helper.exe");
  }
  const std::wstring versionDir = fullPath(parentPath(helperPath));
  const std::wstring versionsDir = fullPath(parentPath(versionDir));
  if (!equalOrdinalIgnoreCase(baseName(versionsDir), L"versions") ||
      equalOrdinalIgnoreCase(versionDir, versionsDir)) {
    throw Error("active helper is not in one installed version directory");
  }
  const std::wstring installRoot = fullPath(parentPath(versionsDir));
  const std::wstring serviceDir = fullPath(installRoot + L"\\service");
  const std::wstring binDir = fullPath(installRoot + L"\\bin");
  ensureSafeParent(parentPath(helperPath));
  ensureSafeParent(serviceDir);
  ensureSafeParent(binDir);

  auto updaterOwner = serviceSidForAccount(kUpdaterServiceAccount);
  const std::wstring releaseSddl =
      updaterArtifactSddl(L"release", updaterOwner.data(), false);
  const auto proveLockedFile = [&](
      HANDLE file,
      const std::wstring& expectedPath,
      const std::wstring& expectedSddl) {
    FILE_ATTRIBUTE_TAG_INFO attributes{};
    BY_HANDLE_FILE_INFORMATION information{};
    if (!GetFileInformationByHandleEx(
            file, FileAttributeTagInfo, &attributes, sizeof(attributes)) ||
        !GetFileInformationByHandle(file, &information) ||
        (attributes.FileAttributes &
         (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)) ||
        information.nNumberOfLinks != 1 ||
        !equalOrdinalIgnoreCase(
            finalDosPath(file, "GetFinalPathNameByHandleW(trusted install file)"),
            expectedPath)) {
      throw Error("trusted install file is not a unique exact regular file");
    }
    requireExactFileSecurity(file, expectedSddl);
  };

  Handle helper(CreateFileW(
      helperPath.c_str(), GENERIC_READ | READ_CONTROL, FILE_SHARE_READ,
      nullptr, OPEN_EXISTING,
      FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_SEQUENTIAL_SCAN, nullptr));
  if (!helper) failWin("CreateFileW(active helper module)");
  proveLockedFile(helper.get(), helperPath, releaseSddl);

  const std::wstring installRootPath =
      fullPath(binDir + L"\\install-root.txt");
  const std::wstring publisherPath =
      fullPath(binDir + L"\\publisher.sha256");
  LockedRegularContents installRootMetadata = readLockedRegularFile(
      installRootPath, 32768, FILE_SHARE_READ);
  LockedRegularContents publisherMetadata = readLockedRegularFile(
      publisherPath, 1024, FILE_SHARE_READ);
  proveLockedFile(
      installRootMetadata.handle.get(), installRootPath, releaseSddl);
  proveLockedFile(
      publisherMetadata.handle.get(), publisherPath, releaseSddl);
  const std::wstring metadataRoot = checkedLocalAbsolutePath(
      trimmedFileText(installRootMetadata.bytes), "stable install root");
  if (!equalOrdinalIgnoreCase(metadataRoot, installRoot)) {
    throw Error("stable install metadata disagrees with the active helper root");
  }
  const std::string publisher = checkedPublisher(fromUtf8(
      trimmedFileText(publisherMetadata.bytes)));

  VerifiedCurrentRoost active =
      verifyCurrentRoost(versionsDir, serviceDir, publisher);
  if (!equalOrdinalIgnoreCase(active.helperPath, helperPath) ||
      !equalOrdinalIgnoreCase(active.versionDir, versionDir)) {
    throw Error("active helper is not the current manifest helper");
  }
  requireExactFileSecurity(
      active.current.handle.get(),
      updaterArtifactSddl(L"current", updaterOwner.data(), false));
  requireExactFileSecurity(active.executable.handle.get(), releaseSddl);
  requireExactFileSecurity(active.helper.handle.get(), releaseSddl);

  ServiceHandle manager = scm();
  ServiceHandle updater(OpenServiceW(
      manager.get(), kUpdaterServiceName,
      SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS));
  if (!updater) failWin("OpenServiceW(trusted updater topology)");
  const ServiceConfigSnapshot config = queryConfig(updater.get());
  const std::vector<std::wstring> configured = splitCommandLine(config.image);
  const auto separator = std::find(configured.begin(), configured.end(), L"--");
  const std::wstring updaterShawl =
      fullPath(active.versionDir + L"\\shawl.exe");
  const std::wstring updaterExecutable =
      fullPath(active.versionDir + L"\\roost.exe");
  if (configured.empty() ||
      separator == configured.end() ||
      static_cast<std::size_t>(configured.end() - separator) != 3 ||
      !equalOrdinalIgnoreCase(
          absoluteLocal(configured.front(), "updater Shawl path"),
          updaterShawl) ||
      !equalOrdinalIgnoreCase(
          absoluteLocal(*(separator + 1), "updater executable path"),
          updaterExecutable) ||
      *(separator + 2) != L"__windows-updater-broker" ||
      !processEnvironmentValue(L"ROOST_SERVICE_ACCOUNT").has_value() ||
      !equalOrdinalIgnoreCase(
          config.account,
          *processEnvironmentValue(L"ROOST_SERVICE_ACCOUNT")) ||
      config.startType != SERVICE_AUTO_START) {
    throw Error("RoostUpdaterV2 SCM topology is not the exact stable topology");
  }
  return installRoot;
}

void enableInstallerRestorePrivilege() {
  HANDLE raw = nullptr;
  if (!OpenProcessToken(
          GetCurrentProcess(), TOKEN_QUERY | TOKEN_ADJUST_PRIVILEGES, &raw)) {
    failWin("OpenProcessToken(installer privilege)");
  }
  Handle token(raw);
  TOKEN_PRIVILEGES privileges{};
  privileges.PrivilegeCount = 1;
  if (!LookupPrivilegeValueW(
          nullptr, SE_RESTORE_NAME, &privileges.Privileges[0].Luid)) {
    failWin("LookupPrivilegeValueW(SeRestorePrivilege)");
  }
  privileges.Privileges[0].Attributes = SE_PRIVILEGE_ENABLED;
  SetLastError(ERROR_SUCCESS);
  if (!AdjustTokenPrivileges(
          token.get(), FALSE, &privileges, 0, nullptr, nullptr) ||
      GetLastError() == ERROR_NOT_ALL_ASSIGNED) {
    failWin("AdjustTokenPrivileges(SeRestorePrivilege)");
  }
}

std::optional<std::wstring>& installerInstallRootStorage() {
  static std::optional<std::wstring> root;
  return root;
}

std::optional<std::wstring> authorizedInstallerInstallRoot() {
  return installerInstallRootStorage();
}

void requirePinnedInstallerAncestry(const char* operation) {
  if (!elevatedAdministratorContext()) {
    throw Error(
        std::string(operation) +
        " requires RoostUpdaterV2 or a signed elevated installer");
  }
  std::array<wchar_t, MAX_PATH> programDataBuffer{};
  const HRESULT commonAppData = SHGetFolderPathW(
      nullptr, CSIDL_COMMON_APPDATA, nullptr, SHGFP_TYPE_CURRENT,
      programDataBuffer.data());
  if (FAILED(commonAppData) || !programDataBuffer[0]) {
    throw Error("SHGetFolderPathW(CommonAppData) failed");
  }
  const std::wstring programData(programDataBuffer.data());
  const std::wstring installRoot = fullPath(programData + L"\\Roost");

  const std::wstring helperPath =
      checkedUpdaterArtifactAbsolutePath(
          moduleFilePath(), "installer helper module");
  const std::wstring packageRoot = fullPath(parentPath(helperPath));
  const std::wstring installerPath =
      fullPath(packageRoot + L"\\roost.exe");
  if (!equalOrdinalIgnoreCase(baseName(helperPath), L"roost-win-helper.exe") ||
      !equalOrdinalIgnoreCase(baseName(installerPath), L"roost.exe") ||
      !equalOrdinalIgnoreCase(parentPath(installerPath), packageRoot)) {
    throw Error(
        std::string(operation) +
        " requires same-package roost.exe and roost-win-helper.exe");
  }
  ensureSafeParent(packageRoot);

  const auto lockExecutable = [](const std::wstring& path, const char* label) {
    Handle file(CreateFileW(
        path.c_str(), GENERIC_READ | READ_CONTROL, FILE_SHARE_READ,
        nullptr, OPEN_EXISTING,
        FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_SEQUENTIAL_SCAN,
        nullptr));
    if (!file) failWin(label);
    FILE_ATTRIBUTE_TAG_INFO attributes{};
    BY_HANDLE_FILE_INFORMATION information{};
    if (!GetFileInformationByHandleEx(
            file.get(), FileAttributeTagInfo, &attributes,
            sizeof(attributes)) ||
        !GetFileInformationByHandle(file.get(), &information) ||
        (attributes.FileAttributes &
         (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)) ||
        information.nNumberOfLinks != 1 ||
        !equalOrdinalIgnoreCase(
            finalDosPath(
                file.get(),
                "GetFinalPathNameByHandleW(installer executable)"),
            path)) {
      throw Error("installer executable is not a unique exact regular file");
    }
    return file;
  };
  Handle helper = lockExecutable(
      helperPath, "CreateFileW(installer helper module)");
  Handle installer = lockExecutable(
      installerPath, "CreateFileW(installer parent executable)");

  const auto publisherValue =
      processEnvironmentValue(L"ROOST_WINDOWS_PUBLISHER_SHA256");
  if (!publisherValue || publisherValue->empty()) {
    throw Error("signed installer publisher pin is missing");
  }
  const std::string publisher = checkedPublisher(*publisherValue);

  const std::wstring installRootPath =
      fullPath(installRoot + L"\\bin\\install-root.txt");
  const std::wstring publisherPath =
      fullPath(installRoot + L"\\bin\\publisher.sha256");
  const DWORD rootAttributes = GetFileAttributesW(installRootPath.c_str());
  const DWORD rootError = rootAttributes == INVALID_FILE_ATTRIBUTES
      ? GetLastError() : ERROR_SUCCESS;
  const DWORD publisherAttributes = GetFileAttributesW(publisherPath.c_str());
  const DWORD publisherError = publisherAttributes == INVALID_FILE_ATTRIBUTES
      ? GetLastError() : ERROR_SUCCESS;
  const bool rootExists = rootAttributes != INVALID_FILE_ATTRIBUTES;
  const bool publisherExists = publisherAttributes != INVALID_FILE_ATTRIBUTES;
  if (!rootExists && rootError != ERROR_FILE_NOT_FOUND &&
      rootError != ERROR_PATH_NOT_FOUND) {
    failWin("GetFileAttributesW(installer root metadata)", rootError);
  }
  if (!publisherExists && publisherError != ERROR_FILE_NOT_FOUND &&
      publisherError != ERROR_PATH_NOT_FOUND) {
    failWin("GetFileAttributesW(installer publisher metadata)", publisherError);
  }
  if (rootExists) {
    LockedRegularContents metadata = readLockedRegularFile(
        installRootPath, 32768, FILE_SHARE_READ);
    BY_HANDLE_FILE_INFORMATION information{};
    if (!GetFileInformationByHandle(metadata.handle.get(), &information) ||
        information.nNumberOfLinks != 1 ||
        !equalOrdinalIgnoreCase(
            finalDosPath(
                metadata.handle.get(),
                "GetFinalPathNameByHandleW(installer root metadata)"),
            installRootPath) ||
        !equalOrdinalIgnoreCase(
            checkedLocalAbsolutePath(
                trimmedFileText(metadata.bytes), "stable install root"),
            installRoot)) {
      throw Error("existing stable install-root metadata disagrees with bootstrap");
    }
  }
  if (publisherExists) {
    LockedRegularContents metadata = readLockedRegularFile(
        publisherPath, 1024, FILE_SHARE_READ);
    BY_HANDLE_FILE_INFORMATION information{};
    if (!GetFileInformationByHandle(metadata.handle.get(), &information) ||
        information.nNumberOfLinks != 1 ||
        !equalOrdinalIgnoreCase(
            finalDosPath(
                metadata.handle.get(),
                "GetFinalPathNameByHandleW(installer publisher metadata)"),
            publisherPath) ||
        checkedPublisher(fromUtf8(trimmedFileText(metadata.bytes))) != publisher) {
      throw Error("existing stable publisher metadata disagrees with bootstrap");
    }
  }

  inspectAuthenticode(helperPath, publisher);
  inspectAuthenticode(installerPath, publisher);
  enableInstallerRestorePrivilege();
  installerInstallRootStorage() = installRoot;
}

bool elevatedAdministratorContext() {
  HANDLE raw = nullptr;
  if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw)) {
    failWin("OpenProcessToken(elevation)");
  }
  Handle token(raw);
  TOKEN_ELEVATION elevation{};
  DWORD returned = 0;
  if (!GetTokenInformation(
          token.get(), TokenElevation, &elevation, sizeof(elevation),
          &returned)) {
    failWin("GetTokenInformation(TokenElevation)");
  }
  std::array<std::uint8_t, SECURITY_MAX_SID_SIZE> administrators{};
  DWORD bytes = static_cast<DWORD>(administrators.size());
  if (!CreateWellKnownSid(
          WinBuiltinAdministratorsSid, nullptr,
          administrators.data(), &bytes)) {
    failWin("CreateWellKnownSid(Administrators)");
  }
  return elevation.TokenIsElevated != 0 &&
      tokenContainsEnabledSid(administrators.data());
}

void requireUpdaterOrElevatedInstallerContext(const char* operation) {
  if (runningInUpdaterServiceContext()) return;
  if (runningInServiceContext(kKeeperServiceName) ||
      runningInServiceContext(kWorkerServiceName) ||
      runningInServiceContext(kCoordinatorServiceName)) {
    throw Error(
        std::string(operation) +
        " is forbidden from this Roost service role");
  }
  requirePinnedInstallerAncestry(operation);
}

void requireUpdaterServiceContext(const char* operation) {
  if (!runningInUpdaterServiceContext()) {
    throw Error(
        std::string(operation) +
        " requires RoostUpdaterV2 service ancestry");
  }
}

UpdaterRequestCaller requireUpdaterRequestServiceContext() {
  const bool worker = runningInServiceContext(kWorkerServiceName);
  const bool coordinator =
      runningInServiceContext(kCoordinatorServiceName);
  if (worker && coordinator) {
    throw Error(
        "create-updater-request has ambiguous Worker/Coordinator ancestry");
  }
  if (worker) return UpdaterRequestCaller::Worker;
  if (coordinator) return UpdaterRequestCaller::Coordinator;
  if (runningInServiceContext(kKeeperServiceName) ||
      runningInUpdaterServiceContext()) {
    throw Error(
        "create-updater-request is not available to Keeper or Updater");
  }
  return UpdaterRequestCaller::Interactive;
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

std::wstring requiredEnvironmentPath(const wchar_t* name) {
  DWORD needed = GetEnvironmentVariableW(name, nullptr, 0);
  if (!needed) failWin("GetEnvironmentVariableW");
  std::vector<wchar_t> buffer(needed);
  const DWORD written =
      GetEnvironmentVariableW(name, buffer.data(), needed);
  if (!written || written + 1 != needed) failWin("GetEnvironmentVariableW");
  std::wstring path(buffer.data(), written);
  if (path.size() < 3 || !std::iswalpha(path[0]) || path[1] != L':' ||
      (path[2] != L'\\' && path[2] != L'/')) {
    throw Error("service directory environment path is not local and absolute");
  }
  return fullPath(path);
}

void requireHealthPipeClosedAfterFrame(HANDLE pipe) {
  const auto deadline =
      std::chrono::steady_clock::now() + std::chrono::seconds(2);
  for (;;) {
    DWORD available = 0;
    if (!PeekNamedPipe(pipe, nullptr, 0, nullptr, &available, nullptr)) {
      const DWORD code = GetLastError();
      if (code == ERROR_BROKEN_PIPE ||
          code == ERROR_PIPE_NOT_CONNECTED ||
          code == ERROR_NO_DATA) {
        return;
      }
      failWin("PeekNamedPipe(service health)", code);
    }
    if (available) {
      throw Error("service health connection contained multiple frames");
    }
    if (std::chrono::steady_clock::now() >= deadline) {
      throw Error("service health pipe did not close after its response frame");
    }
    Sleep(10);
  }
}

void probeServiceHealth(const std::vector<std::wstring>& args) {
  expect(args, 2, "probe-service-health <service> <named-pipe>");
  if (args[0] != kWorkerServiceName &&
      args[0] != kCoordinatorServiceName) {
    throw Error("service health probe service is not allowlisted");
  }
  static const std::wstring pipePrefix = L"\\\\.\\pipe\\";
  if (args[1].size() <= pipePrefix.size() ||
      !equalOrdinalIgnoreCase(
          args[1].substr(0, pipePrefix.size()), pipePrefix)) {
    throw Error("service health endpoint must be a local named pipe");
  }
  std::vector<std::uint8_t> request = framedInput(64U * 1024U - 4U);
  const JsonValue requestJson = JsonParser(std::string_view(
      reinterpret_cast<const char*>(request.data()), request.size())).parse();
  if (requestJson.type != JsonValue::Type::Object) {
    throw Error("service health request must be a JSON object");
  }

  const std::wstring serviceDir = requiredEnvironmentPath(L"ROOST_SERVICE_DIR");
  if (!equalOrdinalIgnoreCase(baseName(serviceDir), L"service")) {
    throw Error("ROOST_SERVICE_DIR does not name the install service directory");
  }
  const std::wstring installRoot = fullPath(parentPath(serviceDir));
  const std::wstring versionsRoot = fullPath(installRoot + L"\\versions");
  const std::wstring binDir = fullPath(installRoot + L"\\bin");
  ensureSafeParent(serviceDir);
  ensureSafeParent(versionsRoot);
  ensureSafeParent(binDir);
  LockedRegularContents installRootMetadata = readLockedRegularFile(
      binDir + L"\\install-root.txt", 32768, FILE_SHARE_READ);
  LockedRegularContents publisherMetadata = readLockedRegularFile(
      binDir + L"\\publisher.sha256", 1024, FILE_SHARE_READ);
  const std::wstring configuredRoot = checkedLocalAbsolutePath(
      trimmedFileText(installRootMetadata.bytes), "stable install root");
  if (!equalOrdinalIgnoreCase(configuredRoot, installRoot)) {
    throw Error("ROOST_SERVICE_DIR disagrees with stable install-root metadata");
  }
  const std::string publisher = checkedPublisher(fromUtf8(
      trimmedFileText(publisherMetadata.bytes)));
  VerifiedCurrentRoost active =
      verifyCurrentRoost(versionsRoot, serviceDir, publisher);

  ServiceHandle manager = scm();
  ServiceHandle service(OpenServiceW(
      manager.get(),
      args[0].c_str(),
      SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS));
  if (!service) failWin("OpenServiceW(service health)");
  const ServiceConfigSnapshot config = queryConfig(service.get());
  const std::vector<std::wstring> configuredArgv =
      splitCommandLine(config.image);
  if (configuredArgv.empty() ||
      lower(baseName(configuredArgv[0])) != L"shawl.exe") {
    throw Error("health service is not SCM-configured through Shawl");
  }
  const SERVICE_STATUS_PROCESS status = queryStatus(service.get());
  if (status.dwCurrentState != SERVICE_RUNNING ||
      !status.dwProcessId ||
      lower(baseName(processImage(status.dwProcessId))) != L"shawl.exe") {
    throw Error("health service SCM Shawl process is not running");
  }

  if (!WaitNamedPipeW(args[1].c_str(), 2000)) {
    failWin("WaitNamedPipeW(service health)");
  }
  Handle pipe(CreateFileW(
      args[1].c_str(),
      GENERIC_READ | GENERIC_WRITE,
      0,
      nullptr,
      OPEN_EXISTING,
      FILE_ATTRIBUTE_NORMAL,
      nullptr));
  if (!pipe) failWin("CreateFileW(service health pipe)");
  ULONG serverPid = 0;
  if (!GetNamedPipeServerProcessId(pipe.get(), &serverPid) ||
      !serverPid) {
    failWin("GetNamedPipeServerProcessId");
  }
  const auto parents = parentMap();
  const auto parent = parents.find(serverPid);
  if (parent == parents.end() ||
      parent->second != status.dwProcessId) {
    throw Error("health pipe server is not the direct child of its SCM Shawl process");
  }
  Handle serverProcess(OpenProcess(
      PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE,
      FALSE,
      serverPid));
  if (!serverProcess) failWin("OpenProcess(service health server)");
  std::wstring serverImage(32768, L'\0');
  DWORD serverImageLength = static_cast<DWORD>(serverImage.size());
  if (!QueryFullProcessImageNameW(
      serverProcess.get(),
      0,
      serverImage.data(),
      &serverImageLength)) {
    failWin("QueryFullProcessImageNameW(service health server)");
  }
  serverImage.resize(serverImageLength);
  if (!equalOrdinalIgnoreCase(fullPath(serverImage), active.executablePath)) {
    throw Error("health pipe server is not the immutable active roost.exe");
  }

  sendFrame(pipe.get(), request);
  std::vector<std::uint8_t> response =
      readFrame(pipe.get(), 64U * 1024U - 4U);
  requireHealthPipeClosedAfterFrame(pipe.get());
  const std::string payload(
      reinterpret_cast<const char*>(response.data()), response.size());
  (void)fromUtf8(payload);
  const SERVICE_STATUS_PROCESS finalStatus = queryStatus(service.get());
  if (finalStatus.dwCurrentState != SERVICE_RUNNING ||
      finalStatus.dwProcessId != status.dwProcessId ||
      WaitForSingleObject(serverProcess.get(), 0) != WAIT_TIMEOUT) {
    throw Error("health service process identity changed during the probe");
  }
  emit("{\"serverPid\":" + std::to_string(serverPid) +
      ",\"payloadUtf8\":" + json(payload) + "}");
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
bool relocationIdentifier(const std::wstring& value) {
  if (value.size() != 36) return false;
  for (std::size_t index = 0; index < value.size(); ++index) {
    if (index == 8 || index == 13 || index == 18 || index == 23) {
      if (value[index] != L'-') return false;
    } else if (!std::iswxdigit(value[index])) return false;
  }
  return true;
}

bool regularRelocationFile(
    const std::wstring& path,
    const std::wstring& trustedRoot,
    ACCESS_MASK access,
    DWORD share,
    Handle* opened = nullptr) {
  Handle file(CreateFileW(
      path.c_str(), access, share, nullptr, OPEN_EXISTING,
      FILE_FLAG_OPEN_REPARSE_POINT, nullptr));
  if (!file) {
    const DWORD code = GetLastError();
    if (code == ERROR_FILE_NOT_FOUND || code == ERROR_PATH_NOT_FOUND) return false;
    failWin("CreateFileW(coordinator relocation file)", code);
  }
  FILE_ATTRIBUTE_TAG_INFO attributes{};
  BY_HANDLE_FILE_INFORMATION information{};
  if (!GetFileInformationByHandleEx(
          file.get(), FileAttributeTagInfo, &attributes, sizeof(attributes)) ||
      !GetFileInformationByHandle(file.get(), &information)) {
    failWin("GetFileInformationByHandle(coordinator relocation file)");
  }
  if ((attributes.FileAttributes &
       (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)) ||
      information.nNumberOfLinks != 1) {
    throw Error("coordinator relocation path is not a unique regular file");
  }
  const std::wstring resolved = finalDosPath(
      file.get(), "GetFinalPathNameByHandleW(coordinator relocation file)");
  if (!pathAtOrBelow(trustedRoot, resolved) ||
      !equalOrdinalIgnoreCase(resolved, fullPath(path))) {
    throw Error("coordinator relocation file escaped its exact trusted path");
  }
  if (opened) *opened = std::move(file);
  return true;
}

void ensurePrivateRelocationDirectory(const std::wstring& path) {
  if (!CreateDirectoryW(path.c_str(), nullptr)) {
    const DWORD code = GetLastError();
    if (code != ERROR_ALREADY_EXISTS) {
      failWin("CreateDirectoryW(coordinator relocation)", code);
    }
  }
  Handle directory(CreateFileW(
      path.c_str(), READ_CONTROL | WRITE_DAC | WRITE_OWNER,
      FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
      nullptr, OPEN_EXISTING,
      FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
      nullptr));
  if (!directory) {
    failWin("CreateFileW(coordinator relocation directory)");
  }
  FILE_ATTRIBUTE_TAG_INFO attributes{};
  if (!GetFileInformationByHandleEx(
          directory.get(), FileAttributeTagInfo, &attributes,
          sizeof(attributes)) ||
      !(attributes.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) ||
      (attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) ||
      !equalOrdinalIgnoreCase(
          finalDosPath(
              directory.get(),
              "GetFinalPathNameByHandleW(coordinator relocation directory)"),
          fullPath(path))) {
    throw Error("coordinator relocation directory is not exact");
  }
  auto owner = serviceSidForAccount(kUpdaterServiceAccount);
  setAndVerifyFileSecurity(
      directory.get(), updaterArtifactSddl(L"private", owner.data(), true));
}

void writePrivateRelocationFile(
    const std::wstring& path,
    const std::vector<std::uint8_t>& contents) {
  Handle file(CreateFileW(
      path.c_str(), GENERIC_READ | GENERIC_WRITE | READ_CONTROL |
          WRITE_DAC | WRITE_OWNER,
      0, nullptr, CREATE_ALWAYS,
      FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT |
          FILE_FLAG_WRITE_THROUGH, nullptr));
  if (!file) failWin("CreateFileW(coordinator relocation control)");
  BY_HANDLE_FILE_INFORMATION information{};
  if (!GetFileInformationByHandle(file.get(), &information) ||
      information.nNumberOfLinks != 1 ||
      !equalOrdinalIgnoreCase(
          finalDosPath(file.get(), "GetFinalPathNameByHandleW(coordinator relocation control)"),
          fullPath(path))) {
    throw Error("coordinator relocation control file is not exact");
  }
  writeAll(file.get(), contents.data(), contents.size());
  if (!FlushFileBuffers(file.get())) failWin("FlushFileBuffers(coordinator relocation control)");
  auto owner = serviceSidForAccount(kUpdaterServiceAccount);
  setAndVerifyFileSecurity(
      file.get(), updaterArtifactSddl(L"private", owner.data(), false));
}

std::wstring readRelocationText(HANDLE file) {
  const std::vector<std::uint8_t> bytes =
      readUpdaterArtifactContents(file, 64U * 1024U);
  return fromUtf8(std::string_view(
      reinterpret_cast<const char*>(bytes.data()), bytes.size()));
}

std::wstring readRelocationText(const std::wstring& path) {
  Handle file;
  if (!regularRelocationFile(
          path, parentPath(path), GENERIC_READ | READ_CONTROL,
          FILE_SHARE_READ, &file)) {
    throw Error("coordinator relocation control file is missing");
  }
  return readRelocationText(file.get());
}

std::wstring coordinatorRelocationFileSddl(bool rollback) {
  auto owner = currentSid();
  const auto coordinator = serviceSidForName(kCoordinatorServiceName);
  const auto updater = serviceSidForName(kUpdaterServiceName);
  std::wstring sddl = L"O:" + sidText(owner.data()) + L"D:P";
  appendDirectoryAllow(sddl, false, L"FA", L"SY");
  appendDirectoryAllow(sddl, false, L"FA", L"BA");
  appendDirectoryAllow(
      sddl, false, L"FA",
      sidText(const_cast<std::uint8_t*>(coordinator.data())));
  appendDirectoryAllow(
      sddl, false, rollback ? L"0x0017019f" : L"GR",
      sidText(const_cast<std::uint8_t*>(updater.data())));
  return sddl;
}
Handle openExactRelocationDirectory(
    const std::wstring& path,
    ACCESS_MASK access,
    DWORD* volume = nullptr) {
  Handle directory(CreateFileW(
      path.c_str(), access, FILE_SHARE_READ | FILE_SHARE_WRITE |
          FILE_SHARE_DELETE,
      nullptr, OPEN_EXISTING,
      FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS, nullptr));
  if (!directory) failWin("CreateFileW(exact relocation directory)");
  FILE_ATTRIBUTE_TAG_INFO attributes{};
  BY_HANDLE_FILE_INFORMATION information{};
  if (!GetFileInformationByHandleEx(
          directory.get(), FileAttributeTagInfo, &attributes,
          sizeof(attributes)) ||
      !GetFileInformationByHandle(directory.get(), &information) ||
      !(attributes.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) ||
      (attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) ||
      !equalOrdinalIgnoreCase(
          finalDosPath(
              directory.get(),
              "GetFinalPathNameByHandleW(exact relocation directory)"),
          fullPath(path))) {
    throw Error("relocation directory identity is not exact");
  }
  ensureSafeParent(path);
  if (volume) *volume = information.dwVolumeSerialNumber;
  return directory;
}

std::wstring coordinatorStageSddl(bool admissionRoot) {
  const auto worker = serviceSidForName(kWorkerServiceName);
  const auto updater = serviceSidForName(kUpdaterServiceName);
  auto owner = serviceSidForAccount(kUpdaterServiceAccount);
  std::wstring sddl = L"O:" + sidText(owner.data()) + L"D:P";
  appendDirectoryAllow(sddl, true, L"FA", L"SY");
  appendDirectoryAllow(sddl, true, L"FA", L"BA");
  appendDirectoryAllow(
      sddl, true, kDirectoryModifyRights,
      sidText(const_cast<std::uint8_t*>(worker.data())));
  appendDirectoryAllow(
      sddl, !admissionRoot, admissionRoot ? L"0x001200a4" : L"GR",
      sidText(const_cast<std::uint8_t*>(updater.data())));
  return sddl;
}

void ensureCoordinatorStageDirectory(
    const std::wstring& path,
    bool admissionRoot) {
  const std::wstring sddl = coordinatorStageSddl(admissionRoot);
  PSECURITY_DESCRIPTOR raw = nullptr;
  if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
          sddl.c_str(), SDDL_REVISION_1, &raw, nullptr)) {
    failWin(
        "ConvertStringSecurityDescriptorToSecurityDescriptorW(coordinator stage)");
  }
  Local<SECURITY_DESCRIPTOR> descriptor(
      static_cast<SECURITY_DESCRIPTOR*>(raw));
  SECURITY_ATTRIBUTES security{};
  security.nLength = sizeof(security);
  security.lpSecurityDescriptor = descriptor.get();
  if (!CreateDirectoryW(path.c_str(), &security)) {
    const DWORD code = GetLastError();
    if (code != ERROR_ALREADY_EXISTS) {
      failWin("CreateDirectoryW(coordinator stage)", code);
    }
  }
  Handle directory =
      openExactRelocationDirectory(path, READ_CONTROL);
  requireExactFileSecurity(directory.get(), sddl);
}

void removeIncompleteRelocationTree(const std::wstring& path) {
  const DWORD attributes = GetFileAttributesW(path.c_str());
  if (attributes == INVALID_FILE_ATTRIBUTES) {
    const DWORD code = GetLastError();
    if (code == ERROR_FILE_NOT_FOUND || code == ERROR_PATH_NOT_FOUND) return;
    failWin("GetFileAttributesW(incomplete relocation tree)", code);
  }
  std::vector<SecurityTreeEntry> entries =
      openSecurityTree(path, DELETE | READ_CONTROL);
  std::stable_sort(
      entries.begin(), entries.end(),
      [](const SecurityTreeEntry& left, const SecurityTreeEntry& right) {
        return left.relative.size() > right.relative.size();
      });
  for (auto& entry : entries) {
    FILE_DISPOSITION_INFO disposition{};
    disposition.DeleteFile = TRUE;
    if (!SetFileInformationByHandle(
            entry.handle.get(), FileDispositionInfo,
            &disposition, sizeof(disposition))) {
      failWin("SetFileInformationByHandle(incomplete relocation tree)");
    }
    entry.handle.reset();
  }
}

void requireSameRelocationVolume(HANDLE file, DWORD volume) {
  BY_HANDLE_FILE_INFORMATION information{};
  if (!GetFileInformationByHandle(file, &information)) {
    failWin("GetFileInformationByHandle(relocation volume)");
  }
  if (information.dwVolumeSerialNumber != volume) {
    throw Error("coordinator relocation crossed a volume boundary");
  }
}


void coordinatorRelocationState(const std::vector<std::wstring>& args) {
  expect(args, 3, "coordinator-relocation-state <action> <relocation-id> <handoff-id>");
  requireUpdaterServiceContext("coordinator-relocation-state");
  if (!relocationIdentifier(args[1]) || !relocationIdentifier(args[2])) {
    throw Error("coordinator relocation identity is not a UUID");
  }
  const UpdaterArtifactRoots roots = updaterArtifactRoots();
  const std::wstring coordinator = fullPath(roots.service + L"\\data\\coordinator");
  const std::wstring workerRelocation =
      fullPath(roots.service + L"\\data\\worker\\relocation");
  const std::wstring workerStage =
      fullPath(workerRelocation + L"\\" + args[2]);
  const std::wstring rollbackRoot =
      fullPath(roots.updater + L"\\coordinator-relocation");
  const std::wstring rollback = fullPath(rollbackRoot + L"\\" + args[1]);
  const std::wstring building = rollback + L".building";
  const std::wstring marker = rollback + L"\\prepared";
  const std::array<std::pair<const wchar_t*, const wchar_t*>, 6> files{{
      {L"coordinator_v2.snapshot", L"coordinator_v2.db"},
      {L"ssh_ed25519.key", L"ssh_ed25519.key"},
      {L"authorized_keys.roost", L"authorized_keys.roost"},
      {L"target-handoff.json", L"coord-handoff.json"},
      {nullptr, L"coordinator_v2.db-wal"},
      {nullptr, L"coordinator_v2.db-shm"},
  }};

  if (args[0] == L"admit-stage") {
    ensureCoordinatorStageDirectory(workerRelocation, true);
    ensureCoordinatorStageDirectory(workerStage, false);
    emit("{\"action\":\"admit-stage\",\"durable\":true}");
    return;
  }

  if (args[0] == L"prepare") {
    Handle existingMarker;
    if (regularRelocationFile(
            marker, rollback, GENERIC_READ | READ_CONTROL,
            FILE_SHARE_READ, &existingMarker)) {
      if (readRelocationText(existingMarker.get()) != args[2]) {
        throw Error("coordinator relocation rollback identity changed");
      }
      emit("{\"action\":\"prepare\",\"durable\":true}");
      return;
    }
    std::vector<SecurityTreeEntry> staged =
        openSecurityTree(workerStage, GENERIC_READ | READ_CONTROL);
    std::set<std::wstring> allowed{
        L"", L"prepared.json", L"coordinator_v2.snapshot",
        L"ssh_ed25519.key", L"authorized_keys.roost", L"target-handoff.json"};
    for (const auto& entry : staged) {
      if (!allowed.erase(lower(entry.relative))) {
        throw Error("coordinator relocation staging tree is not exact");
      }
    }
    if (allowed.contains(L"coordinator_v2.snapshot") ||
        allowed.contains(L"ssh_ed25519.key") ||
        allowed.contains(L"authorized_keys.roost") ||
        allowed.contains(L"target-handoff.json")) {
      throw Error("coordinator relocation staging tree is incomplete");
    }
    ensurePrivateRelocationDirectory(rollbackRoot);
    removeIncompleteRelocationTree(rollback);
    removeIncompleteRelocationTree(building);
    ensurePrivateRelocationDirectory(building);
    DWORD coordinatorVolume = 0;
    Handle coordinatorParent = openExactRelocationDirectory(
        coordinator, 0x001200e2, &coordinatorVolume);
    Handle buildingParent = openExactRelocationDirectory(
        building, GENERIC_WRITE | READ_CONTROL | SYNCHRONIZE);
    auto privateOwner = serviceSidForAccount(kUpdaterServiceAccount);
    const std::wstring privateSddl =
        updaterArtifactSddl(L"private", privateOwner.data(), false);
    for (std::size_t index = 0; index < files.size(); ++index) {
      const auto& item = files[index];
      const std::wstring destination =
          fullPath(coordinator + L"\\" + item.second);
      Handle prior;
      if (regularRelocationFile(
              destination, coordinator, GENERIC_READ | READ_CONTROL,
              FILE_SHARE_READ, &prior)) {
        requireSameRelocationVolume(prior.get(), coordinatorVolume);
        streamHeldArtifactToAtomicDestination(
            prior.get(), building + L"\\" + item.second + L".prior",
            buildingParent.get(), privateSddl,
            "coordinator relocation backup");
        const std::string sddl = toUtf8(objectSecuritySddl(prior.get()));
        writePrivateRelocationFile(
            building + L"\\" + item.second + L".sddl",
            std::vector<std::uint8_t>(sddl.begin(), sddl.end()));
      } else {
        writePrivateRelocationFile(
            building + L"\\" + item.second + L".absent", {});
      }
      if (index < 4) {
        Handle source;
        if (!regularRelocationFile(
                fullPath(workerStage + L"\\" + item.first),
                workerStage, GENERIC_READ | READ_CONTROL,
                FILE_SHARE_READ, &source)) {
          throw Error("coordinator relocation staged source disappeared");
        }
        requireSameRelocationVolume(source.get(), coordinatorVolume);
        streamHeldArtifactToAtomicDestination(
            source.get(), building + L"\\admitted-" + item.second,
            buildingParent.get(), privateSddl,
            "coordinator relocation admitted source");
      }
    }
    const std::string identity = toUtf8(args[2]);
    writePrivateRelocationFile(
        building + L"\\prepared",
        std::vector<std::uint8_t>(identity.begin(), identity.end()));
    if (!FlushFileBuffers(buildingParent.get())) {
      failWin("FlushFileBuffers(coordinator relocation building)");
    }
    buildingParent.reset();
    coordinatorParent.reset();
    if (!MoveFileExW(
            building.c_str(), rollback.c_str(),
            MOVEFILE_WRITE_THROUGH)) {
      failWin("MoveFileExW(coordinator rollback publish)");
    }
    Handle rollbackParent = openExactRelocationDirectory(
        rollbackRoot, GENERIC_WRITE | READ_CONTROL | SYNCHRONIZE);
    if (!FlushFileBuffers(rollbackParent.get())) {
      failWin("FlushFileBuffers(coordinator rollback parent)");
    }
    if (readRelocationText(marker) != args[2]) {
      throw Error("published coordinator rollback identity is not exact");
    }
    emit("{\"action\":\"prepare\",\"durable\":true}");
    return;
  }

  if (readRelocationText(marker) != args[2]) {
    throw Error("coordinator relocation rollback identity changed");
  }
  DWORD coordinatorVolume = 0;
  Handle coordinatorParent = openExactRelocationDirectory(
      coordinator, 0x001200e2, &coordinatorVolume);
  if (args[0] == L"promote") {
    for (std::size_t index = 0; index < 4; ++index) {
      Handle source;
      if (!regularRelocationFile(
              rollback + L"\\admitted-" + files[index].second,
              rollback, GENERIC_READ | READ_CONTROL,
              FILE_SHARE_READ, &source)) {
        throw Error("verified updater relocation source is missing");
      }
      requireSameRelocationVolume(source.get(), coordinatorVolume);
      std::optional<std::wstring> priorSddl;
      Handle priorDescriptor;
      const std::wstring priorDescriptorPath =
          rollback + L"\\" + files[index].second + L".sddl";
      if (regularRelocationFile(
              priorDescriptorPath, rollback, GENERIC_READ | READ_CONTROL,
              FILE_SHARE_READ, &priorDescriptor)) {
        priorSddl = readRelocationText(priorDescriptor.get());
      }
      streamHeldArtifactToAtomicDestination(
          source.get(), fullPath(coordinator + L"\\" + files[index].second),
          coordinatorParent.get(), coordinatorRelocationFileSddl(true),
          "coordinator relocation promotion", true, priorSddl);
    }
    for (std::size_t index = 4; index < files.size(); ++index) {
      Handle stale;
      if (regularRelocationFile(
              fullPath(coordinator + L"\\" + files[index].second),
              coordinator, DELETE | READ_CONTROL, 0, &stale)) {
        FILE_DISPOSITION_INFO disposition{};
        disposition.DeleteFile = TRUE;
        if (!SetFileInformationByHandle(
                stale.get(), FileDispositionInfo,
                &disposition, sizeof(disposition))) {
          failWin("SetFileInformationByHandle(coordinator sidecar)");
        }
      }
    }
    if (!FlushFileBuffers(coordinatorParent.get())) {
      failWin("FlushFileBuffers(coordinator relocation parent)");
    }
  } else if (args[0] == L"restore") {
    for (const auto& item : files) {
      const std::wstring destination =
          fullPath(coordinator + L"\\" + item.second);
      Handle prior;
      if (regularRelocationFile(
              rollback + L"\\" + item.second + L".prior",
              rollback, GENERIC_READ | READ_CONTROL,
              FILE_SHARE_READ, &prior)) {
        requireSameRelocationVolume(prior.get(), coordinatorVolume);
        Handle descriptor;
        if (!regularRelocationFile(
                rollback + L"\\" + item.second + L".sddl",
                rollback, GENERIC_READ | READ_CONTROL,
                FILE_SHARE_READ, &descriptor)) {
          throw Error("coordinator relocation rollback descriptor is missing");
        }
        streamHeldArtifactToAtomicDestination(
            prior.get(), destination, coordinatorParent.get(),
            readRelocationText(descriptor.get()),
            "coordinator relocation restore", true,
            coordinatorRelocationFileSddl(true));
      } else {
        Handle created;
        if (regularRelocationFile(
                destination, coordinator, DELETE | READ_CONTROL,
                0, &created)) {
          FILE_DISPOSITION_INFO disposition{};
          disposition.DeleteFile = TRUE;
          if (!SetFileInformationByHandle(
                  created.get(), FileDispositionInfo,
                  &disposition, sizeof(disposition))) {
            failWin("SetFileInformationByHandle(coordinator restore)");
          }
        }
      }
    }
    if (!FlushFileBuffers(coordinatorParent.get())) {
      failWin("FlushFileBuffers(coordinator relocation parent)");
    }
  } else if (args[0] == L"commit") {
    const std::wstring committedSddl =
        coordinatorRelocationFileSddl(false);
    for (std::size_t index = 0; index < 4; ++index) {
      const std::wstring path =
          fullPath(coordinator + L"\\" + files[index].second);
      Handle readable;
      if (!regularRelocationFile(
              path, coordinator, READ_CONTROL,
              FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
              &readable)) {
        throw Error("committed coordinator state is incomplete");
      }
      if (fileSecurityMatchesExact(readable.get(), committedSddl)) {
        continue;
      }
      readable.reset();
      Handle writable;
      if (!regularRelocationFile(
              path, coordinator, READ_CONTROL | WRITE_DAC | WRITE_OWNER,
              FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
              &writable)) {
        throw Error("committed coordinator state is incomplete");
      }
      setAndVerifyFileSecurity(writable.get(), committedSddl);
    }
  } else {
    throw Error("coordinator relocation state action is not allowlisted");
  }
  emit("{\"action\":" + json(args[0]) + ",\"durable\":true}");
}


}  // namespace roost

int wmain(int argc, wchar_t** argv) {
  SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
  try {
    const std::wstring selfPath = roost::moduleFilePath();
    if (roost::lower(roost::baseName(selfPath)) == L"roost.exe") {
      const int firstForwarded =
          argc > 1 && std::wstring(argv[1]) == L"launch-current" ? 2 : 1;
      std::vector<std::wstring> forwarded;
      forwarded.reserve(static_cast<std::size_t>(
          std::max(argc - firstForwarded, 0)));
      for (int index = firstForwarded; index < argc; ++index) {
        forwarded.emplace_back(argv[index]);
      }
      return roost::launchCurrentRoost(selfPath, forwarded);
    }
    if (argc < 2) throw roost::Error("usage: roost-win-helper <operation> [arguments]");
    const std::wstring operation = argv[1];
    std::vector<std::wstring> arguments;
    arguments.reserve(static_cast<std::size_t>(argc - 2));
    for (int index = 2; index < argc; ++index) arguments.emplace_back(argv[index]);

    if (operation == L"launch-current") {
      return roost::launchCurrentRoost(selfPath, arguments);
    } else if (operation == L"version" || operation == L"--version") {
      roost::expect(arguments, 0, "version");
      roost::emit(
          "{\"protocol\":1,\"helper\":\"roost-win-helper\",\"arch\":\"x64\",\"commands\":["
          "\"version\",\"launch-current\",\"flush-file\",\"replace-file\",\"remove-file\",\"apply-dacl\","
          "\"apply-account-dacl\",\"apply-artifact-dacl\",\"protect-updater-artifact\","
          "\"prepare-updater-artifact\",\"create-updater-request\",\"consume-updater-request\","
          "\"read-updater-artifact\",\"replace-updater-artifact\",\"remove-updater-artifact\","
          "\"inspect-updater-artifact\",\"copy-updater-artifact\","
          "\"coordinator-relocation-state\","
          "\"snapshot-file-security-tree\","
          "\"restore-file-security-tree\",\"protect-directory\",\"protect-directory-tree\","
          "\"protect-service-health\",\"get-dacl\",\"apply-sddl\",\"probe-exclusive-open\","
          "\"current-user-sid\","
          "\"host-sample\",\"process-snapshot\",\"listening-ports\",\"verify-cms-detached\","
          "\"verify-authenticode\",\"extract-zip\",\"assert-service-context\",\"probe-service-health\","
          "\"resolve-account-sid\",\"resolve-service-sid\",\"grant-logon-as-service\","
          "\"apply-service-dacl\",\"revoke-service-dacl\",\"configure-service-sid\","
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
    } else if (operation == L"apply-account-dacl") {
      roost::applyAccountDaclCommand(arguments);
    } else if (operation == L"apply-artifact-dacl") {
      roost::applyArtifactDaclCommand(arguments);
    } else if (operation == L"protect-updater-artifact") {
      roost::protectUpdaterArtifact(arguments);
    } else if (operation == L"prepare-updater-artifact") {
      roost::prepareUpdaterArtifact(arguments);
    } else if (operation == L"create-updater-request") {
      roost::createUpdaterRequest(arguments);
    } else if (operation == L"consume-updater-request") {
      roost::consumeUpdaterRequest(arguments);
    } else if (operation == L"read-updater-artifact") {
      roost::readUpdaterArtifact(arguments);
    } else if (operation == L"replace-updater-artifact") {
      roost::replaceUpdaterArtifact(arguments);
    } else if (operation == L"remove-updater-artifact") {
      roost::removeUpdaterArtifact(arguments);
    } else if (operation == L"inspect-updater-artifact") {
      roost::inspectUpdaterArtifact(arguments);
    } else if (operation == L"copy-updater-artifact") {
      roost::copyUpdaterArtifact(arguments);
    } else if (operation == L"coordinator-relocation-state") {
      roost::coordinatorRelocationState(arguments);
    } else if (operation == L"snapshot-file-security-tree") {
      roost::requireUpdaterOrElevatedInstallerContext(
          "snapshot-file-security-tree");
      roost::snapshotFileSecurityTree(arguments);
    } else if (operation == L"restore-file-security-tree") {
      roost::requireUpdaterOrElevatedInstallerContext(
          "restore-file-security-tree");
      roost::restoreFileSecurityTree(arguments);
    } else if (operation == L"protect-directory") {
      roost::protectDirectory(arguments);
    } else if (operation == L"protect-directory-tree") {
      roost::requireUpdaterOrElevatedInstallerContext(
          "protect-directory-tree");
      roost::protectDirectoryTree(arguments);
    } else if (operation == L"protect-service-health") {
      roost::protectServiceHealth(arguments);
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
    } else if (operation == L"resolve-service-sid") {
      roost::resolveServiceSid(arguments);
    } else if (operation == L"grant-logon-as-service") {
      roost::grantLogonAsService(arguments);
    } else if (operation == L"apply-service-dacl") {
      roost::applyServiceDacl(arguments);
    } else if (operation == L"revoke-service-dacl") {
      roost::revokeServiceDacl(arguments);
    } else if (operation == L"configure-service-sid") {
      roost::configureServiceSid(arguments);
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
    } else if (operation == L"probe-service-health") {
      roost::probeServiceHealth(arguments);
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
