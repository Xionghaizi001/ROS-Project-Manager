# Deliberately no ROS: replay must bootstrap a clean target.
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive LANG=C.UTF-8
RUN apt-get update && apt-get install -y --no-install-recommends \
    sudo ca-certificates curl gnupg git build-essential cmake python3 locales \
    && locale-gen en_US.UTF-8 \
    && update-locale LANG=en_US.UTF-8 \
    && (if id ubuntu >/dev/null 2>&1; then userdel -r ubuntu; fi) \
    && useradd -m -u 1000 -s /bin/bash tester \
    && echo 'tester ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/tester \
    && printf 'Acquire::Retries "3";\n' > /etc/apt/apt.conf.d/80-retries \
    && rm -rf /var/lib/apt/lists/*
# QEMU binfmt may omit credential propagation; run apt in the disposable
# root container without changing host-wide setuid emulation policy.
USER root
WORKDIR /home/tester/project
