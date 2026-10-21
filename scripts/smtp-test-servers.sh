#!/usr/bin/env bash
# Start the SMTP test servers for crates/delivery case D03 (MailHog + two smtp4dev) in Docker and
# print the environment variables that make the e-mail tests run against them.
#   eval "$(scripts/smtp-test-servers.sh up)"   # start and export SIP_FAXD_TEST_SMTP*
#   cargo test -p delivery --test email
#   scripts/smtp-test-servers.sh down
# Without these variables the container-based D03 cases print "skip" and pass; the cases that use the
# scripted in-process SMTP server always run. CI uses the same images as job services (ci.yml).
set -euo pipefail
case "${1:-up}" in
  up)
    docker rm -f sipfax-mailhog sipfax-s4d-starttls sipfax-s4d-smtps >/dev/null 2>&1 || true
    docker run -d --name sipfax-mailhog -p 31025:1025 -p 31026:8025 mailhog/mailhog >/dev/null
    docker run -d --name sipfax-s4d-starttls -p 31125:25 -p 31080:80 \
      -e ServerOptions__TlsMode=StartTls -e ServerOptions__AuthenticationRequired=true \
      -e ServerOptions__SmtpAllowAnyCredentials=false -e ServerOptions__Users__0__Username=faxuser \
      -e ServerOptions__Users__0__Password=faxpass rnwood/smtp4dev >/dev/null
    docker run -d --name sipfax-s4d-smtps -p 31465:25 -p 31081:80 \
      -e ServerOptions__TlsMode=ImplicitTls rnwood/smtp4dev >/dev/null
    for u in http://127.0.0.1:31026/api/v2/messages http://127.0.0.1:31080/api/version http://127.0.0.1:31081/api/version; do
      for _ in $(seq 1 60); do curl -fs "$u" >/dev/null && break; sleep 2; done
      curl -fs "$u" >/dev/null || { echo "not ready: $u" >&2; exit 1; }
    done
    cat <<VARS
export SIP_FAXD_TEST_SMTP=127.0.0.1:31025 SIP_FAXD_TEST_SMTP_API=http://127.0.0.1:31026
export SIP_FAXD_TEST_SMTP_STARTTLS=127.0.0.1:31125 SIP_FAXD_TEST_SMTP_STARTTLS_API=http://127.0.0.1:31080
export SIP_FAXD_TEST_SMTP_SMTPS=127.0.0.1:31465 SIP_FAXD_TEST_SMTP_SMTPS_API=http://127.0.0.1:31081
VARS
    ;;
  down) docker rm -f sipfax-mailhog sipfax-s4d-starttls sipfax-s4d-smtps >/dev/null 2>&1 || true ;;
  *) echo "usage: $0 [up|down]" >&2; exit 2 ;;
esac
