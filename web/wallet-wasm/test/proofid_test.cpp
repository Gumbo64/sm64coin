// proofid_test VECTORS.jsonl: sm64coin::proof_id (sm64coin_wasm.cpp) against the ids the Rust
// crate computed natively (pow/sm64pow/examples/proofid_vectors_v04.rs, vectors-v04.jsonl). Prints a line per mismatch and a total.
#include <cstdio>
#include <cstring>
#include <fstream>
#include <iostream>
#include <string>
#include "sm64coin/sm64coin.h"

static std::string unhex(const std::string &s)
{
  std::string o;
  for (size_t i = 0; i + 1 < s.size(); i += 2) o.push_back((char)std::stoi(s.substr(i, 2), nullptr, 16));
  return o;
}
static std::string field(const std::string &l, const char *k)
{
  const std::string key = std::string("\"") + k + "\":\"";
  const size_t a = l.find(key) + key.size();
  return l.substr(a, l.find('"', a) - a);
}
int main(int argc, char **argv)
{
  std::ifstream f(argv[1]);
  std::string line;
  int n = 0, bad = 0;
  while (std::getline(f, line))
  {
    if (line.empty()) continue;
    const crypto::hash h = sm64coin::proof_id(unhex(field(line, "blob")));
    char hex[65];
    for (int i = 0; i < 32; ++i) snprintf(hex + 2 * i, 3, "%02x", (unsigned char)h.data[i]);
    ++n;
    if (field(line, "id") != hex) { ++bad; printf("MISMATCH %s wasm=%s native=%s\n", field(line, "name").c_str(), hex, field(line, "id").c_str()); }
  }
  printf("%d vectors, %d mismatches\n", n, bad);
  return bad != 0;
}
