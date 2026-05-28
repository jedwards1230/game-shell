FROM archlinux:base-devel

# Hyprland + Wayland runtime
RUN pacman -Syu --noconfirm && \
    pacman -S --noconfirm \
      hyprland wayvnc grim \
      python python-pip python-numpy \
      qt6-base qt6-declarative qt6-wayland qt6-shadertools qt6-svg \
      cmake ninja git jemalloc pipewire pam cli11 \
      wayland wayland-protocols libdrm mesa \
      dbus python-yaml \
      ttf-dejavu noto-fonts-emoji && \
    pacman -Scc --noconfirm

# noVNC web viewer + websockify
RUN pip install --break-system-packages websockify && \
    git clone --depth 1 https://github.com/novnc/noVNC.git /opt/novnc && \
    rm -rf /opt/novnc/.git

# Build Quickshell from source
ARG QUICKSHELL_REF=master
RUN git clone https://github.com/outfoxxed/quickshell.git /tmp/quickshell-src && \
    cd /tmp/quickshell-src && git checkout ${QUICKSHELL_REF} && \
    cmake -GNinja -B build \
      -DCMAKE_BUILD_TYPE=Release \
      -DCMAKE_INSTALL_PREFIX=/usr/local \
      -DUSE_JEMALLOC=ON \
      -DCRASH_HANDLER=OFF && \
    cmake --build build -j$(nproc) && \
    cmake --install build && \
    rm -rf /tmp/quickshell-src

# Runtime user + XDG runtime dir
RUN useradd -m -s /bin/bash -u 1000 dev && \
    mkdir -p /run/user/1000 && chmod 0700 /run/user/1000 && chown dev:dev /run/user/1000 && \
    mkdir -p /home/dev/.config/quickshell && chown dev:dev /home/dev/.config/quickshell

COPY dev/stubs/ /usr/local/bin/stubs/
COPY dev/entrypoint.sh dev/restart.sh /usr/local/bin/
RUN chmod +x /usr/local/bin/entrypoint.sh /usr/local/bin/restart.sh \
    /usr/local/bin/stubs/*
COPY dev/hyprland-dev.conf /etc/hyprland-dev.conf
COPY dev/targets.json /opt/game-shell-defaults/targets.json

ENV PATH="/usr/local/bin/stubs:$PATH"
ENV WLR_BACKENDS=headless
ENV WLR_LIBINPUT_NO_DEVICES=1
ENV XDG_RUNTIME_DIR=/run/user/1000
ENV WAYLAND_DISPLAY=wayland-1
ENV QT_QPA_PLATFORM=wayland

USER dev
WORKDIR /opt/game-shell
EXPOSE 5900 6080
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
