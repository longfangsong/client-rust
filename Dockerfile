FROM golang
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs > ./install.sh && sh ./install.sh -y
RUN apt update && apt install -y curl git cmake libssl-dev
WORKDIR /
RUN git clone https://github.com/longfangsong/client-rust.git
WORKDIR /client-rust
RUN git checkout -qf 37f1bed79a414f5edbda3347dcf81476ca86e923
RUN /root/.cargo/bin/cargo build --all
