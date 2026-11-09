Test-only certificates for the HTTPS tests in `src/tls.rs`. The private keys protect nothing.

- `selfsigned.pem` / `.key`: a self-signed certificate, as made by `openssl req -x509`.
- `ca.pem`: a private CA; `leaf.pem` / `.key`: a certificate for `127.0.0.1` it signed.
- `other.pem`: an unrelated certificate the test server never presents.

All are P-256 and valid until 2126. To regenerate:

```sh
ec="-newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes"
openssl req -x509 $ec -days 36500 -subj /CN=qbittui-selfsigned -keyout selfsigned.key -out selfsigned.pem
openssl req -x509 $ec -days 36500 -subj /CN=qbittui-test-ca -keyout ca.key -out ca.pem
openssl req $ec -subj /CN=127.0.0.1 -keyout leaf.key -out leaf.csr
printf 'subjectAltName=IP:127.0.0.1\nbasicConstraints=CA:FALSE\nkeyUsage=digitalSignature\nextendedKeyUsage=serverAuth\n' > leaf.ext
openssl x509 -req -in leaf.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 36500 -extfile leaf.ext -out leaf.pem
openssl req -x509 $ec -days 36500 -subj /CN=qbittui-other -keyout /dev/null -out other.pem
rm leaf.csr leaf.ext ca.key ca.srl
```
