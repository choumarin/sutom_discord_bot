use chrono::{DateTime, Local, TimeZone, Timelike};
use lazy_static::lazy_static;
use regex::Regex;
use serenity::futures::StreamExt;
use serenity::model::channel::Message;
use serenity::model::gateway::Ready;
use serenity::model::prelude::UserId;
use serenity::model::user::User;
use serenity::model::Timestamp;
use serenity::prelude::*;
use serenity::{async_trait, model::prelude::ChannelId};
use std::collections::HashMap;
use std::env;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;

#[derive(Debug)]
#[allow(dead_code)]
struct Score {
    tries: usize,
    secs: usize,
}

type DailyScores = HashMap<User, Score>;

struct Leaderboard;

impl TypeMapKey for Leaderboard {
    type Value = Arc<RwLock<HashMap<usize, DailyScores>>>;
}

struct Config;

struct Conf {
    sutom_channel: ChannelId,
    admin_id: UserId,
}

impl TypeMapKey for Config {
    type Value = Conf;
}

struct Handler;

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.id == ctx.http.get_current_user().await.unwrap().id {
            // Don't react to self
            return;
        }
        if msg.content == "!scan_all" {
            if msg.author.id
                == ctx
                    .data
                    .read()
                    .await
                    .get::<Config>()
                    .expect("Config must exist")
                    .admin_id
            {
                println!("Admin message:\n{:?}", msg);
                let typing = msg.channel_id.start_typing(&ctx.http).unwrap();
                scan_all(&ctx, &msg.channel_id).await.unwrap();
                typing.stop().unwrap();
                // msg.channel_id
                //     .say(&ctx.http, "all done")
                //     .await
                //     .expect("to send message");
            } else {
                msg.channel_id
                    .say(&ctx.http, "ah ah ah you didn't say the magic word")
                    .await
                    .expect("to send message");
            }
        }
        let re = Regex::new(r"!scores?(?:\s*#?\s*(?P<grid_id>\d+))?").unwrap();
        if let Some(caps) = re.captures(&msg.content) {
            if let Some(grid_id) = caps.name("grid_id") {
                send_scores_for_grid(
                    &ctx,
                    &msg.channel_id,
                    grid_id.as_str().parse::<usize>().expect("a number"),
                )
                .await;
            } else {
                send_scores_now(&ctx, &msg.channel_id).await;
            }
        }
        if msg.content == "!all_time" {
            send_all_time_scores(&ctx, &msg.channel_id).await;
        }
        scan_message(&ctx, &msg).await;
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
        let ctx_read = ctx.data.read().await;
        let sutom_channel = ctx_read
            .get::<Config>()
            .expect("Config must exist")
            .sutom_channel;
        scan_all(&ctx, &sutom_channel).await.unwrap();

        daily_message(&ctx, &sutom_channel).await;
    }
}

async fn daily_message(ctx: &Context, channel: &ChannelId) {
    let mut interval = time::interval(Duration::from_secs(1));
    let mut ran = false;
    loop {
        interval.tick().await;
        let local: DateTime<Local> = Local::now();
        let is_midnight = local.time().hour() == 0 && local.time().minute() == 0;
        if is_midnight {
            if !ran {
                send_scores_before(ctx, channel).await;
                ran = true;
            }
        } else {
            ran = false;
        }
        // println!("{:?}", ran);
    }
}

async fn send_scores_before(ctx: &Context, channel: &ChannelId) {
    scan_2_days(ctx, channel).await.unwrap();

    let grid_id = (sutom_grid_number() - 1) as usize;
    send_scores_for_grid(ctx, channel, grid_id).await;
}

async fn send_scores_now(ctx: &Context, channel: &ChannelId) {
    scan_2_days(ctx, channel).await.unwrap();

    let grid_id = sutom_grid_number() as usize;
    send_scores_for_grid(ctx, channel, grid_id).await;
}

async fn send_scores_for_grid(ctx: &Context, channel: &ChannelId, grid_id: usize) {
    let data_read = ctx.data.read().await;

    let leaderboard = data_read
        .get::<Leaderboard>()
        .expect("Leaderboard should exist")
        .read()
        .await;

    let response = match pp_daily(&leaderboard, grid_id) {
        Ok(s) => s,
        Err(e) => e,
    };
    if let Err(why) = channel.say(&ctx.http, response).await {
        println!("Error sending message: {:?}", why);
    }
}

async fn send_all_time_scores(ctx: &Context, channel: &ChannelId) {
    let data_read = ctx.data.read().await;

    let leaderboard = data_read
        .get::<Leaderboard>()
        .expect("Leaderboard should exist")
        .read()
        .await;

    let all_time = make_all_time(&leaderboard);

    if let Err(why) = channel.say(&ctx.http, pp_all_time(&all_time)).await {
        println!("Error sending message: {:?}", why);
    }
}

type Position = usize;

fn make_all_time(
    all_time: &HashMap<usize, DailyScores>,
) -> HashMap<User, HashMap<Position, usize>> {
    let mut all_time_board: HashMap<User, HashMap<Position, usize>> = HashMap::new();
    for game in all_time.values() {
        let ordered = ordered_daily_scores_by_secs(game);
        for (position, (user, _)) in ordered.iter().enumerate() {
            let user_table = all_time_board.entry((*user).clone()).or_default();
            *user_table.entry(position).or_default() += 1;
        }
    }
    all_time_board
}

async fn scan_all(ctx: &Context, channel: &ChannelId) -> Result<(), String> {
    println!("Start scan");
    let res = scan_since(ctx, channel, Timestamp::from_unix_timestamp(0).unwrap()).await;
    println!("End scan");
    res
}

async fn scan_2_days(ctx: &Context, channel: &ChannelId) -> Result<(), String> {
    scan_since(
        ctx,
        channel,
        Timestamp::from_unix_timestamp(Timestamp::now().unix_timestamp() - 48 * 3600).unwrap(),
    )
    .await
}

async fn scan_since(ctx: &Context, channel: &ChannelId, ts_from: Timestamp) -> Result<(), String> {
    let mut messages = channel.messages_iter(&ctx.http).boxed();

    while let Some(message_result) = messages.next().await {
        if let Ok(message) = message_result {
            // println!(
            //     "{}@{} : {}",
            //     message.author.name, message.timestamp, message.content
            // );
            scan_message(ctx, &message).await;
            if message.timestamp.unix_timestamp() < ts_from.unix_timestamp() {
                break;
            }
        }
    }
    Ok(())
}

async fn scan_message(ctx: &Context, message: &Message) {
    // get only a read lock on the whole data
    let data_read = ctx.data.read().await;

    // clone the ARC (which is just a pointer), so that we can close the read lock
    let leaderboard = data_read
        .get::<Leaderboard>()
        .expect("Leaderboard should exist")
        .clone();

    // get a write lock on just the leaderboard
    let mut leaderboard_lock = leaderboard.write().await;

    if let Some((id, score)) = extract_score(&message.content) {
        let daily_scores = leaderboard_lock.entry(id).or_default();

        daily_scores.insert((message.author).clone(), score);
    }
}

fn pp_daily(all_time: &HashMap<usize, DailyScores>, grid_id: usize) -> Result<String, String> {
    if let Some(daily_scores) = all_time.get(&grid_id) {
        let ordered = ordered_daily_scores_by_secs(daily_scores);
        let str = pretty_print_daily_ordered(ordered);
        return Ok(format!("Meilleurs temps #{}\n{}", grid_id, str));
    }
    Err(format!("Pas de temps pour grille #{}", grid_id))
}

fn extract_score(message: &str) -> Option<(usize, Score)> {
    lazy_static! {
        static ref RE: Regex = Regex::new(r"SUTOM #(\d+) (\d)/6 (\d+)?h?(\d{2}):(\d{2})").unwrap();
    }
    if let Some(caps) = RE.captures(message) {
        let re_to_usize = |key| caps.get(key).unwrap().as_str().parse::<usize>().unwrap();
        let id = re_to_usize(1);
        let hours = caps
            .get(3)
            .map_or(0, |v| v.as_str().parse::<usize>().unwrap());
        let mins = re_to_usize(4);
        let mut total_sec = re_to_usize(5);
        total_sec += mins * 60;
        total_sec += hours * 60;
        let score = Score {
            tries: re_to_usize(2),
            secs: total_sec,
        };

        return Some((id, score));
    }
    None
}

fn pp_secs(secs: usize) -> String {
    let seconds = secs % 60;
    let minutes = (secs / 60) % 60;
    let hours = (secs / 60) / 60;
    let mut str = format!("{:02}:{:02}", minutes, seconds);
    if hours > 0 {
        str = format!("{}h", hours) + &str;
    }
    str
}

fn pretty_print_daily_ordered(ordered: Vec<(&User, &Score)>) -> String {
    // 🟦🟥
    let mut str = String::new();
    // if let Some(rank) = ordered.get(1) {
    //     str += &format!("🟦🟥 {} {}\n", rank.0, pp_secs(rank.1.secs));
    // }
    // if let Some(rank) = ordered.get(0) {
    //     str += &format!("🟦🟦🟥 {} {}\n", rank.0, pp_secs(rank.1.secs));
    // }
    // if let Some(rank) = ordered.get(2) {
    //     str += &format!("🟥 {} {}\n", rank.0, pp_secs(rank.1.secs));
    // }
    if let Some(rank) = ordered.get(0) {
        writeln!(str, "1. 🥇 {} {}", pp_secs(rank.1.secs), rank.0.mention()).unwrap();
    }
    if let Some(rank) = ordered.get(1) {
        writeln!(str, "2. 🥈 {} {}", pp_secs(rank.1.secs), rank.0.mention()).unwrap();
    }
    if let Some(rank) = ordered.get(2) {
        writeln!(str, "3. 🥉 {} {}", pp_secs(rank.1.secs), rank.0.mention()).unwrap();
    }
    for (i, rank) in ordered.iter().enumerate().skip(3) {
        writeln!(
            str,
            "{}.       {} {}",
            i,
            pp_secs(rank.1.secs),
            rank.0.mention()
        )
        .unwrap();
    }
    str
}

fn ordered_daily_scores_by_secs(daily_scores: &HashMap<User, Score>) -> Vec<(&User, &Score)> {
    let mut vec: Vec<(&User, &Score)> = daily_scores.iter().collect();
    vec.sort_by(|a, b| a.1.secs.cmp(&b.1.secs));
    vec
}

fn sutom_grid_number() -> i64 {
    let date_grid = Local::today().and_hms(0, 0, 0).timestamp_millis();
    let date_origin = Local.ymd(2022, 1, 7).and_hms(0, 0, 0).timestamp_millis();
    ((date_grid - date_origin) / (24 * 3600 * 1000)) + 1
}

fn order_all_time(
    all_time_board: &HashMap<User, HashMap<Position, usize>>,
) -> Vec<(&User, &HashMap<Position, usize>)> {
    let mut vec: Vec<(&User, &HashMap<Position, usize>)> = all_time_board.iter().collect();
    vec.sort_by(|a, b| {
        let ha = a.1;
        let hb = b.1;
        if ha.get(&0).unwrap_or(&0) == hb.get(&0).unwrap_or(&0) {
            if ha.get(&1).unwrap_or(&0) == hb.get(&1).unwrap_or(&0) {
                ha.get(&2).unwrap_or(&0).cmp(hb.get(&2).unwrap_or(&0))
            } else {
                ha.get(&1).unwrap_or(&0).cmp(hb.get(&1).unwrap_or(&0))
            }
        } else {
            ha.get(&0).unwrap_or(&0).cmp(hb.get(&0).unwrap_or(&0))
        }
    });
    vec.reverse();
    vec
}

fn pp_all_time(all_time_board: &HashMap<User, HashMap<Position, usize>>) -> String {
    let mut str = String::from("Classement général (temps):\n");
    for (idx, (user, table)) in order_all_time(all_time_board).iter().enumerate() {
        writeln!(
            str,
            "{}. {} 🥇x{} 🥈x{} 🥉x{}",
            idx + 1,
            user.mention(),
            table.get(&0).unwrap_or(&0),
            table.get(&1).unwrap_or(&0),
            table.get(&2).unwrap_or(&0)
        )
        .unwrap();
    }
    str
}

#[cfg(test)]
mod tests {
    use serenity::model::prelude::UserId;

    use super::*;

    fn make_fake_user(id: u64, name: &str) -> User {
        let mut user = User::default();
        user.id = UserId(id);
        user.name = name.to_string();
        user
    }

    #[test]
    fn it_sorts() {
        let mut scores = HashMap::new();
        scores.insert(make_fake_user(1, "alice"), Score { tries: 2, secs: 9 });
        scores.insert(make_fake_user(2, "bob"), Score { tries: 1, secs: 10 });
        scores.insert(make_fake_user(3, "charly"), Score { tries: 3, secs: 8 });

        let ordered = ordered_daily_scores_by_secs(&scores);

        let names: Vec<&String> = ordered.iter().map(|x| &x.0.name).collect();
        assert_eq!(names, vec!["charly", "alice", "bob"]);
    }

    #[test]
    fn it_s_pretty() {
        let mut scores = HashMap::new();
        scores.insert(make_fake_user(1, "alice"), Score { tries: 2, secs: 9 });
        scores.insert(make_fake_user(2, "bob"), Score { tries: 1, secs: 10 });
        scores.insert(make_fake_user(3, "chary"), Score { tries: 3, secs: 8 });
        scores.insert(make_fake_user(4, "dorothy"), Score { tries: 5, secs: 80 });

        let ordered = ordered_daily_scores_by_secs(&scores);

        println!("{}", pretty_print_daily_ordered(ordered));
    }

    #[test]
    fn grid_id() {
        println!("{:?}", sutom_grid_number());
    }

    #[test]
    fn it_makes_all_time() {
        let mut leaderboard = HashMap::new();
        let mut day1 = DailyScores::new();
        day1.insert(make_fake_user(1, "alice"), Score { tries: 2, secs: 9 });
        day1.insert(make_fake_user(2, "bob"), Score { tries: 1, secs: 10 });
        day1.insert(make_fake_user(3, "chary"), Score { tries: 3, secs: 12 });
        let mut day2 = DailyScores::new();
        day2.insert(make_fake_user(1, "alice"), Score { tries: 2, secs: 14 });
        day2.insert(make_fake_user(2, "bob"), Score { tries: 1, secs: 1 });
        day2.insert(make_fake_user(3, "chary"), Score { tries: 3, secs: 2 });
        let mut day3 = DailyScores::new();
        day3.insert(make_fake_user(1, "alice"), Score { tries: 2, secs: 7 });
        day3.insert(make_fake_user(2, "bob"), Score { tries: 1, secs: 19 });
        day3.insert(make_fake_user(3, "chary"), Score { tries: 3, secs: 32 });
        leaderboard.insert(1, day1);
        leaderboard.insert(2, day2);
        leaderboard.insert(3, day3);

        let all_time_board = make_all_time(&leaderboard);
        println!("{}", pp_all_time(&all_time_board));
    }
}

#[tokio::main]
async fn main() {
    // Configure the client with your Discord bot token in the environment.
    let token = env::var("DISCORD_TOKEN").expect("Expected a token in the environment");
    let sutom_channel_id = env::var("SUTOM_CHAN")
        .expect("Expected a channel id in the environment")
        .parse::<u64>()
        .expect("channel to be a number");
    let admin_user_id = env::var("ADMIN_ID")
        .expect("Expected a user id in the environment")
        .parse::<u64>()
        .expect("admin id to be a number");
    // Set gateway intents, which decides what events the bot will be notified about
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;

    let leaderboard = Arc::new(RwLock::new(HashMap::default()));

    // Create a new instance of the Client, logging in as a bot. This will
    // automatically prepend your bot token with "Bot ", which is a requirement
    // by Discord for bot users.
    let mut client = Client::builder(&token, intents)
        .event_handler(Handler)
        .await
        .expect("Err creating client");
    {
        let mut data = client.data.write().await;
        data.insert::<Leaderboard>(leaderboard);
        let conf = Conf {
            sutom_channel: ChannelId(sutom_channel_id),
            admin_id: UserId(admin_user_id),
        };
        data.insert::<Config>(conf);
    }

    // Finally, start a single shard, and start listening to events.
    //
    // Shards will automatically attempt to reconnect, and will perform
    // exponential backoff until it reconnects.
    if let Err(why) = client.start().await {
        println!("Client error: {:?}", why);
    }
}
