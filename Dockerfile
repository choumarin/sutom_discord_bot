FROM rust:buster as builder

WORKDIR /code
RUN git clone --depth 1 https://github.com/choumarin/sutom_discord_bot.git && cd sutom_discord_bot && cargo build --release

FROM debian:buster-slim

COPY --from=builder /code/sutom_discord_bot/target/release/sutom-dbot sutom-dbot

CMD ["./sutom-dbot"]
