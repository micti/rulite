Self-signed `localhost` / `127.0.0.1` test certificate for `tls.test.mjs` and the `tls.rs` unit tests.
Test-only: the private key is public. `other-key.pem` does not match `cert.pem`.

```sh
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -keyout key.pem -out cert.pem \
  -days 36500 -subj /CN=localhost -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" \
  -addext "basicConstraints=critical,CA:TRUE"
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:prime256v1 -out other-key.pem
```
