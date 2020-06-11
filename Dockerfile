FROM golang
ENV http_proxy http://host.docker.internal:1087
ENV https_proxy http://host.docker.internal:1087
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs > ./install.sh && sh ./install.sh -y
RUN apt update && apt install -y curl git cmake libssl-dev
WORKDIR /
RUN git clone https://github.com/longfangsong/client-rust.git
WORKDIR /client-rust
RUN git checkout test
RUN /root/.cargo/bin/cargo build --all
