use chrono::{TimeZone, Timelike, Utc};
use chrono_tz::US::Pacific;
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time;

#[derive(Debug, PartialEq)]
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

struct Handler {
    is_daily_running: AtomicBool,
}

use serde::Deserialize;
use serenity::http::{request::RequestBuilder, routing::RouteInfo, Http};

#[async_trait]
pub trait GlobalName {
    /// Gets the user's global name (aka display name) if set.
    async fn global_name(
        &self,
        http: impl AsRef<Http> + std::marker::Send + std::marker::Sync,
    ) -> String;
}

#[non_exhaustive]
#[derive(Deserialize)]
struct UserWithGlobalName {
    global_name: Option<String>,
}

#[async_trait]
impl GlobalName for User {
    async fn global_name(
        &self,
        http: impl AsRef<Http> + std::marker::Send + std::marker::Sync,
    ) -> String {
        let route_info = RouteInfo::GetUser { user_id: self.id.0 };
        let request = RequestBuilder::new(route_info);

        match http
            .as_ref()
            .fire::<UserWithGlobalName>(request.build())
            .await
        {
            Err(..) => self.name.clone(),
            Ok(u) => u.global_name.unwrap_or(self.name.clone()),
        }
    }
}

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
            send_time_scores(&ctx, &msg.channel_id, 0).await;
        }
        if msg.content == "!last_30" {
            send_time_scores(&ctx, &msg.channel_id, (sutom_grid_number() - 30) as usize).await;
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

        if !self.is_daily_running.load(Ordering::Relaxed) {
            let ctx2 = ctx.clone(); // we can clone, it's just pointers
            tokio::spawn(async move {
                daily_message(&ctx2, &sutom_channel).await;
            });

            self.is_daily_running.swap(true, Ordering::Relaxed);
        }
    }
}

async fn daily_message(ctx: &Context, channel: &ChannelId) {
    let mut interval = time::interval(Duration::from_secs(1));
    let mut ran = false;
    println!("Daily loop starting");
    scan_all(ctx, channel).await.unwrap();
    loop {
        interval.tick().await;
        let now = Utc::now().with_timezone(&Pacific);
        let is_midnight = now.time().hour() == 0 && now.time().minute() == 0;
        if is_midnight {
            if !ran {
                send_scores_before(ctx, channel).await;
                ran = true;
            }
        } else {
            ran = false;
        }
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

    let response = match pp_daily(&leaderboard, grid_id, ctx).await {
        Ok(s) => s,
        Err(e) => e,
    };
    if let Err(why) = channel.say(&ctx.http, response).await {
        println!("Error sending message: {:?}", why);
    }
}

async fn send_time_scores(ctx: &Context, channel: &ChannelId, from_game_id: usize) {
    let data_read = ctx.data.read().await;

    let leaderboard = data_read
        .get::<Leaderboard>()
        .expect("Leaderboard should exist")
        .read()
        .await;

    let all_time = make_times(&leaderboard, from_game_id);

    if let Err(why) = channel
        .say(&ctx.http, pp_times(&all_time, from_game_id, ctx).await)
        .await
    {
        println!("Error sending message: {:?}", why);
    }
}

type Position = usize;

fn make_times(
    all_time: &HashMap<usize, DailyScores>,
    from_game_id: usize,
) -> HashMap<User, HashMap<Position, usize>> {
    let mut time_board: HashMap<User, HashMap<Position, usize>> = HashMap::new();
    for game in all_time.iter().filter_map(|(game_id, game)| {
        if game_id >= &from_game_id && game_id < &(sutom_grid_number() as usize) {
            Some(game)
        } else {
            None
        }
    }) {
        let ordered = ordered_daily_scores_by_secs(game);
        for (position, (user, _)) in ordered.iter().enumerate() {
            let user_table = time_board.entry((*user).clone()).or_default();
            *user_table.entry(position).or_default() += 1;
        }
    }
    time_board
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

async fn pp_daily(
    all_time: &HashMap<usize, DailyScores>,
    grid_id: usize,
    ctx: &Context,
) -> Result<String, String> {
    if let Some(daily_scores) = all_time.get(&grid_id) {
        let ordered = ordered_daily_scores_by_secs(daily_scores);
        let str = pretty_print_daily_ordered(ordered, ctx).await;
        return Ok(format!("Meilleurs temps #{}\n{}", grid_id, str));
    }
    Err(format!("Pas de temps pour grille #{}", grid_id))
}

fn extract_score(message: &str) -> Option<(usize, Score)> {
    lazy_static! {
        static ref RE: Regex =
            Regex::new(r"SUTOM #(?P<id>\d+) (?P<score>\d)/6 (?:(?P<hours>\d+)h)?(?P<minutes>[0-5]\d):(?P<seconds>[0-5]\d)").unwrap();
    }
    let message = message.replace("||", "");
    if let Some(caps) = RE.captures(&message) {
        let re_to_usize = |key| caps.name(key).unwrap().as_str().parse::<usize>().unwrap();
        let id = re_to_usize("id");
        let hours = caps
            .name("hours")
            .map_or(0, |v| v.as_str().parse::<usize>().unwrap());
        let mins = re_to_usize("minutes");
        let mut total_sec = re_to_usize("seconds");
        total_sec += mins * 60;
        total_sec += hours * 60 * 60;
        let score = Score {
            tries: re_to_usize("score"),
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

async fn pretty_print_daily_ordered(ordered: Vec<(&User, &Score)>, ctx: &Context) -> String {
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
        writeln!(
            str,
            "1. 🥇 {} {}",
            pp_secs(rank.1.secs),
            rank.0.global_name(ctx.http.clone()).await
        )
        .unwrap();
    }
    if let Some(rank) = ordered.get(1) {
        writeln!(
            str,
            "2. 🥈 {} {}",
            pp_secs(rank.1.secs),
            rank.0.global_name(ctx.http.clone()).await
        )
        .unwrap();
    }
    if let Some(rank) = ordered.get(2) {
        writeln!(
            str,
            "3. 🥉 {} {}",
            pp_secs(rank.1.secs),
            rank.0.global_name(ctx.http.clone()).await
        )
        .unwrap();
    }
    for (i, rank) in ordered.iter().enumerate().skip(3) {
        writeln!(
            str,
            "{}.       {} {}",
            i + 1,
            pp_secs(rank.1.secs),
            rank.0.global_name(ctx.http.clone()).await
        )
        .unwrap();
    }
    str
}

fn ordered_daily_scores_by_secs(daily_scores: &HashMap<User, Score>) -> Vec<(&User, &Score)> {
    let mut vec: Vec<(&User, &Score)> = daily_scores.iter().collect();
    vec.sort_by(|a, b| {
        if a.1.secs == b.1.secs {
            a.1.tries.cmp(&b.1.tries)
        } else {
            a.1.secs.cmp(&b.1.secs)
        }
    });
    vec
}

fn sutom_grid_number() -> i64 {
    let date_grid = Utc::now()
        .with_timezone(&Pacific)
        .date()
        .and_hms(0, 0, 0)
        .timestamp_millis();
    let date_origin = Utc
        .ymd(2022, 1, 8)
        .with_timezone(&Pacific)
        .and_hms(0, 0, 0)
        .timestamp_millis();
    ((date_grid - date_origin) as f64 / (24 * 3600 * 1000) as f64).round() as i64 + 1
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

async fn pp_times(
    all_time_board: &HashMap<User, HashMap<Position, usize>>,
    from_game_id: usize,
    ctx: &Context,
) -> String {
    let mut str = String::new();
    let since_string = format!(
        "pour les {} derniers jeux",
        sutom_grid_number() - from_game_id as i64
    );
    writeln!(
        str,
        "Classement général (temps) {}:\n",
        if from_game_id == 0 {
            "depuis toujours"
        } else {
            since_string.as_str()
        }
    )
    .unwrap();
    for (idx, (user, table)) in order_all_time(all_time_board).iter().enumerate() {
        writeln!(
            str,
            "{}. {} 🥇x{} 🥈x{} 🥉x{}",
            idx + 1,
            user.global_name(ctx.http.clone()).await,
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
    fn it_sorts_equal() {
        let mut scores = HashMap::new();
        scores.insert(make_fake_user(1, "alice"), Score { tries: 2, secs: 9 });
        scores.insert(make_fake_user(2, "alice2"), Score { tries: 1, secs: 9 });
        scores.insert(make_fake_user(3, "bob"), Score { tries: 1, secs: 10 });
        scores.insert(make_fake_user(4, "charly"), Score { tries: 3, secs: 8 });

        let ordered = ordered_daily_scores_by_secs(&scores);

        let names: Vec<&String> = ordered.iter().map(|x| &x.0.name).collect();
        assert_eq!(names, vec!["charly", "alice2", "alice", "bob"]);
    }

    // #[test]
    // fn it_sorts_really_equal() {
    //     let mut scores = HashMap::new();
    //     scores.insert(make_fake_user(1, "alice"), Score { tries: 2, secs: 9 });
    //     scores.insert(make_fake_user(2, "alice2"), Score { tries: 2, secs: 9 });
    //     scores.insert(make_fake_user(3, "bob"), Score { tries: 1, secs: 10 });
    //     scores.insert(make_fake_user(4, "charly"), Score { tries: 3, secs: 8 });

    //     let ordered = ordered_daily_scores_by_secs(&scores);

    //     let names: Vec<&String> = ordered.iter().map(|x| &x.0.name).collect();
    //     assert_eq!(names, vec!["charly", "alice2", "alice", "bob"]);
    // }

    #[test]
    fn it_parses_scores() {
        let message = "SUTOM #254 5/6 10:56";
        assert_eq!(
            extract_score(message),
            Some((
                254,
                Score {
                    tries: 5,
                    secs: 56 + 60 * 10
                }
            ))
        );
        let message = "SUTOM #254 5/6 2h10:56";
        assert_eq!(
            extract_score(message),
            Some((
                254,
                Score {
                    tries: 5,
                    secs: 56 + 60 * 10 + 2 * 3600
                }
            ))
        );
        let message = "SUTOM #a 5/6 2h10:56";
        assert_eq!(extract_score(message), None);
        let message = "SUTOM #254 x/6 2h10:56";
        assert_eq!(extract_score(message), None);
        let message = "asSUTOM #254 5/6 10:56asd";
        assert_eq!(
            extract_score(message),
            Some((
                254,
                Score {
                    tries: 5,
                    secs: 56 + 60 * 10
                }
            ))
        );
        let message = "||SUTOM #254 5/6 10:56||";
        assert_eq!(
            extract_score(message),
            Some((
                254,
                Score {
                    tries: 5,
                    secs: 56 + 60 * 10
                }
            ))
        );
        let message = "SUTOM #254 5/6 ||10:56||";
        assert_eq!(
            extract_score(message),
            Some((
                254,
                Score {
                    tries: 5,
                    secs: 56 + 60 * 10
                }
            ))
        );
        let message = "SUTOM #254 5/6 510:56";
        assert_eq!(extract_score(message), None);
        let message = "SUTOM #254 5/6 60:56";
        assert_eq!(extract_score(message), None);
        let message = "SUTOM #254 5/6 10:66";
        assert_eq!(extract_score(message), None);
        let message = "#SUTOM #682 6/6 24h01:38";
        assert_eq!(
            extract_score(message),
            Some((
                682,
                Score {
                    tries: 6,
                    secs: 24 * 60 * 60 + 1 * 60 + 38 * 60
                }
            ))
        );
    }

    // TODO: Add mock ctx?
    // #[test]
    // fn it_s_pretty() {
    //     let mut scores = HashMap::new();
    //     scores.insert(make_fake_user(1, "alice"), Score { tries: 2, secs: 9 });
    //     scores.insert(make_fake_user(2, "bob"), Score { tries: 1, secs: 10 });
    //     scores.insert(make_fake_user(3, "chary"), Score { tries: 3, secs: 8 });
    //     scores.insert(make_fake_user(4, "dorothy"), Score { tries: 5, secs: 80 });

    //     let ordered = ordered_daily_scores_by_secs(&scores);

    //     println!("{}", pretty_print_daily_ordered(ordered));
    // }

    #[test]
    fn grid_id() {
        println!("{:?}", sutom_grid_number());
    }

    // TODO: Add mock ctx?
    // #[test]
    // fn it_makes_all_time() {
    //     let mut leaderboard = HashMap::new();
    //     let mut day1 = DailyScores::new();
    //     day1.insert(make_fake_user(1, "alice"), Score { tries: 2, secs: 9 });
    //     day1.insert(make_fake_user(2, "bob"), Score { tries: 1, secs: 10 });
    //     day1.insert(make_fake_user(3, "chary"), Score { tries: 3, secs: 12 });
    //     let mut day2 = DailyScores::new();
    //     day2.insert(make_fake_user(1, "alice"), Score { tries: 2, secs: 14 });
    //     day2.insert(make_fake_user(2, "bob"), Score { tries: 1, secs: 1 });
    //     day2.insert(make_fake_user(3, "chary"), Score { tries: 3, secs: 2 });
    //     let mut day3 = DailyScores::new();
    //     day3.insert(make_fake_user(1, "alice"), Score { tries: 2, secs: 7 });
    //     day3.insert(make_fake_user(2, "bob"), Score { tries: 1, secs: 19 });
    //     day3.insert(make_fake_user(3, "chary"), Score { tries: 3, secs: 32 });
    //     leaderboard.insert(1, day1);
    //     leaderboard.insert(2, day2);
    //     leaderboard.insert(3, day3);

    //     let all_time_board = make_times(&leaderboard, 0);
    //     println!("{}", pp_times(&all_time_board, 0));
    // }

    // TODO: Add mock ctx?
    // #[test]
    // fn it_makes_1_day() {
    //     let mut leaderboard = HashMap::new();
    //     let mut day1 = DailyScores::new();
    //     day1.insert(make_fake_user(1, "alice"), Score { tries: 2, secs: 9 });
    //     day1.insert(make_fake_user(2, "bob"), Score { tries: 1, secs: 10 });
    //     day1.insert(make_fake_user(3, "chary"), Score { tries: 3, secs: 12 });
    //     let mut day2 = DailyScores::new();
    //     day2.insert(make_fake_user(1, "alice"), Score { tries: 2, secs: 14 });
    //     day2.insert(make_fake_user(2, "bob"), Score { tries: 1, secs: 1 });
    //     day2.insert(make_fake_user(3, "chary"), Score { tries: 3, secs: 2 });
    //     let mut day3 = DailyScores::new();
    //     day3.insert(make_fake_user(1, "alice"), Score { tries: 2, secs: 7 });
    //     day3.insert(make_fake_user(2, "bob"), Score { tries: 1, secs: 19 });
    //     day3.insert(make_fake_user(3, "chary"), Score { tries: 3, secs: 32 });
    //     leaderboard.insert(1, day1);
    //     leaderboard.insert(2, day2);
    //     leaderboard.insert(3, day3);

    //     let one_day_board = make_times(&leaderboard, 2);
    //     println!("{}", pp_times(&one_day_board, 2));
    // }
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
        .event_handler(Handler {
            is_daily_running: AtomicBool::new(false),
        })
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
    println!("Starting client v{}", env!("CARGO_PKG_VERSION"));
    if let Err(why) = client.start().await {
        println!("Client error: {:?}", why);
    }
}
