# A TEST judges' image for metric and score judges under the container tier: alpine plus a FAKE
# `swe-metrics` (a shell script that speaks swe-metrics' report and score JSON and logs, into the
# judge scratch, where it ran and what it could reach). It follows the image contract — the tool on
# PATH for any uid, /bin/sh, no credentials, no ENTRYPOINT — so the automated tests exercise the real
# runtime without the real tool. Built offline from the local alpine:3.20.
FROM alpine:3.20
COPY swe-metrics /usr/local/bin/swe-metrics
RUN chmod 0755 /usr/local/bin/swe-metrics
