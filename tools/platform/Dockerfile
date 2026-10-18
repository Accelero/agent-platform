FROM ubuntu:24.04
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    ca-certificates python3 \
    && rm -rf /var/lib/apt/lists/*
COPY downloads.lock.json /tmp/downloads.lock.json
COPY install.py /tmp/install.py
RUN python3 /tmp/install.py && rm /tmp/install.py /tmp/downloads.lock.json
