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
use std::fmt::{format, Formatter, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time;

use charming::component::{Axis, DataZoom, DataZoomType, Title};
use charming::element::{AxisLabel, AxisTick, AxisType, Color};
use charming::series::{Bar, Line};
use charming::theme::Theme;
use charming::{
    component::Legend,
    element::ItemStyle,
    series::{Pie, PieRoseType},
    Chart, ImageFormat, ImageRenderer,
};

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
use serenity::all::{CreateAttachment, CreateMessage};
use serenity::http::Http;
use tempfile::{tempfile, Builder, NamedTempFile, TempPath};

#[async_trait]
pub trait GlobalName {
    /// Gets the user's global name (aka display name) if set.
    async fn global_name(
        &self,
        http: impl AsRef<Http> + std::marker::Send + std::marker::Sync,
    ) -> String;
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
                let typing = msg.channel_id.start_typing(&ctx.http);
                scan_all(&ctx, &msg.channel_id).await.unwrap();
                typing.stop();
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
        if msg.content == "!my_stats" {
            let file = format!("{}.png", msg.author.id);

            let data_read = ctx.data.read().await;

            let leaderboard = data_read
                .get::<Leaderboard>()
                .expect("Leaderboard should exist")
                .read()
                .await;

            let data_raw = user_time_distrib(&leaderboard, &msg.author);
            println!("{:?}", data_raw);

            let data_raw = data_raw
                .into_iter()
                .filter(|(time, _)| *time < 3600usize)
                .collect::<HashMap<_, _>>();

            let max_time = *data_raw.iter().max_by_key(|(time, _)| *time).unwrap().0;
            let max_count = *data_raw.iter().max_by_key(|(_, count)| *count).unwrap().1;
            let mean_time = mean_time(&data_raw);
            let std_dev = std_dev(&data_raw, mean_time);

            dbg!(max_time, max_count, mean_time, std_dev);

            let img = generate_dist_img(
                msg.author.global_name.clone().unwrap(),
                max_time,
                max_count,
                mean_time,
                std_dev,
                &data_raw,
            );

            let paths = [CreateAttachment::path(img).await.unwrap()];

            let builder = CreateMessage::new();
            &msg.channel_id
                .send_files(&ctx.http, paths, builder)
                .await
                .unwrap();
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

fn generate_dist_img(
    username: String,
    max_time: usize,
    max_count: usize,
    mean_time: f64,
    std_dev: f64,
    data: &HashMap<usize, usize>,
) -> TempPath {
    let mean_time = mean_time / 60.0;
    let std_dev = std_dev / 60.0;
    let bell_data: Vec<Vec<_>> = (0..max_time)
        .map(|x| {
            let x = x as f64 / 60.0;
            vec![
                x,
                max_count as f64 * ((-(x - mean_time).powi(2)) / (2.0 * std_dev.powi(2))).exp(),
            ]
        })
        .collect();

    let bar_data = (0..max_time)
        .map(|x| vec![x as f64 / 60.0, (*data.get(&x).unwrap_or(&0)) as f64])
        .collect();
    
    let mean_hours = (mean_time / 60.0).floor();
    let mean_min = (mean_time - mean_hours * 60.0).floor();
    let mean_sec = (mean_time * 60.0 % 60.0).round();

    let chart = Chart::new()
        .title(
            Title::new()
                .text(format!("Games for {username}"))
                .subtext(format!(
                    "Mean: {}h {:#02}m {:#02}s \nStd dev: {}s",
                    mean_hours,
                    mean_min,
                    mean_sec,
                    std_dev * 60.0
                )),
        )
        .background_color(Color::Value("white".to_string()))
        .x_axis(
            Axis::new()
                .type_(AxisType::Value)
                .name("Minutes")
                .axis_label(AxisLabel::new().rotate(45))
                .max_interval(1)
                .min(0)
                .max(30),
        )
        .y_axis(Axis::new().type_(AxisType::Value))
        .series(Line::new().data(bell_data))
        .series(Bar::new().data(bar_data).bar_width(2));

    let mut renderer = ImageRenderer::new(1000, 800).theme(Theme::Default);

    let temp_path = Builder::new()
        .suffix(".png")
        .tempfile()
        .unwrap()
        .into_temp_path();
    renderer
        .save_format(ImageFormat::Png, &chart, "test.png")
        .unwrap();
    renderer
        .save_format(ImageFormat::Png, &chart, &temp_path)
        .unwrap();
    println!("saved");
    temp_path
}

#[test]
fn test_generate_img() {
    let data = HashMap::from([
        (156, 1),
        (218, 1),
        (165, 2),
        (573, 1),
        (136, 4),
        (55, 4),
        (461, 1),
        (113, 5),
        (123, 2),
        (209, 1),
        (7054, 1),
        (35, 6),
        (214, 2),
        (110, 3),
        (105, 1),
        (11256, 1),
        (519, 2),
        (106, 1),
        (255, 2),
        (18, 2),
        (244, 1),
        (3177, 1),
        (225, 2),
        (99, 3),
        (315, 1),
        (830, 1),
        (19, 1),
        (118, 1),
        (613, 1),
        (585, 1),
        (161, 1),
        (9698, 1),
        (2146, 1),
        (112, 8),
        (11, 1),
        (97, 6),
        (73, 3),
        (283, 2),
        (50, 5),
        (1197, 1),
        (513, 1),
        (984, 2),
        (76, 3),
        (51, 10),
        (148, 4),
        (47316, 1),
        (228, 1),
        (169, 4),
        (359, 1),
        (3265, 1),
        (56, 5),
        (391, 1),
        (241, 2),
        (397, 1),
        (473, 1),
        (23353, 1),
        (307, 1),
        (83, 2),
        (100, 2),
        (609, 1),
        (344, 2),
        (222, 1),
        (96, 2),
        (63, 4),
        (38, 2),
        (31, 3),
        (109, 2),
        (180, 2),
        (68, 3),
        (321, 1),
        (140, 1),
        (1863, 1),
        (740, 1),
        (338, 1),
        (3867, 1),
        (90, 4),
        (474, 1),
        (491, 1),
        (2560, 1),
        (554, 1),
        (23202, 1),
        (92, 4),
        (66, 8),
        (331, 1),
        (184, 2),
        (74, 1),
        (362, 1),
        (425, 1),
        (157, 2),
        (186, 2),
        (24, 2),
        (176, 2),
        (129, 3),
        (551, 1),
        (360, 1),
        (192, 1),
        (369, 1),
        (395, 1),
        (628, 1),
        (79, 8),
        (1011, 1),
        (23, 2),
        (205, 1),
        (52, 3),
        (146, 1),
        (82, 3),
        (104, 2),
        (142, 1),
        (921, 1),
        (144, 1),
        (88, 4),
        (322, 1),
        (187, 2),
        (1979, 1),
        (21, 3),
        (837, 1),
        (64, 5),
        (505, 1),
        (496, 1),
        (247, 1),
        (127, 1),
        (906, 1),
        (149, 1),
        (216, 1),
        (178, 2),
        (24425, 1),
        (219, 1),
        (147, 1),
        (34, 8),
        (267, 1),
        (223, 2),
        (193, 1),
        (151, 1),
        (347, 1),
        (954, 1),
        (141, 3),
        (550, 1),
        (1712, 1),
        (199, 1),
        (4039, 1),
        (251, 1),
        (158, 1),
        (2, 1),
        (341, 1),
        (86, 2),
        (542, 1),
        (175, 2),
        (60, 6),
        (254, 1),
        (190, 2),
        (170, 3),
        (2010, 1),
        (13768, 1),
        (1454, 1),
        (196, 1),
        (69, 1),
        (12772, 1),
        (93, 5),
        (114, 5),
        (308, 2),
        (488, 1),
        (365, 2),
        (121, 1),
        (903, 1),
        (137, 2),
        (20, 3),
        (465, 1),
        (432, 1),
        (601, 1),
        (80, 3),
        (45, 1),
        (128, 2),
        (85, 3),
        (212, 1),
        (103, 2),
        (978, 1),
        (1929, 1),
        (2525, 1),
        (78, 1),
        (22, 1),
        (248, 1),
        (3749, 1),
        (28, 3),
        (487, 1),
        (119, 2),
        (256, 1),
        (173, 1),
        (168, 3),
        (389, 1),
        (681, 1),
        (204, 1),
        (75, 4),
        (36, 1),
        (130, 2),
        (115, 1),
        (91, 3),
        (548, 1),
        (164, 3),
        (41, 4),
        (134, 1),
        (25, 5),
        (14016, 1),
        (210, 3),
        (229, 2),
        (396, 1),
        (53, 1),
        (777, 1),
        (59, 3),
        (220, 3),
        (94, 1),
        (377, 2),
        (298, 1),
        (273, 2),
        (131, 3),
        (30, 4),
        (54, 3),
        (523, 1),
        (300, 1),
        (61, 2),
        (686, 1),
        (111, 3),
        (3003, 1),
        (139, 5),
        (117, 3),
        (299, 1),
        (27, 4),
        (181, 1),
        (65, 4),
        (4092, 1),
        (191, 1),
        (29, 2),
        (980, 1),
        (305, 1),
        (101, 2),
        (4, 3),
        (72, 3),
        (188, 1),
        (67, 2),
        (201, 1),
        (174, 3),
        (162, 3),
        (252, 3),
        (16, 2),
        (95, 1),
        (48, 4),
        (166, 2),
        (84, 1),
        (17, 1),
        (58, 4),
        (277, 3),
        (534, 1),
        (358, 2),
        (150, 1),
        (98, 1),
        (26, 2),
        (23494, 1),
        (664, 1),
        (445, 1),
        (120, 1),
        (355, 1),
        (537, 1),
        (126, 1),
        (200, 2),
        (375, 1),
        (233, 1),
        (62, 3),
        (125, 1),
        (42, 2),
        (772, 1),
        (47, 2),
        (37, 5),
        (203, 3),
        (10892, 1),
        (152, 1),
        (539, 1),
        (207, 2),
        (81, 4),
        (89, 4),
        (226, 1),
        (250, 1),
        (364, 1),
        (230, 1),
        (44, 1),
        (172, 3),
        (763, 1),
        (237, 2),
        (253, 1),
        (198, 3),
        (385, 1),
        (440, 1),
        (46, 3),
        (5643, 1),
        (189, 1),
        (258, 1),
        (16785, 1),
        (57, 2),
        (77, 4),
        (259, 1),
        (401, 1),
        (406, 1),
        (135, 2),
        (348, 1),
        (33, 2),
        (122, 3),
        (328, 1),
        (102, 2),
        (143, 3),
        (334, 1),
        (183, 3),
        (459, 1),
        (138, 2),
        (70, 6),
        (295, 1),
        (87, 4),
        (40, 2),
        (133, 1),
        (71, 3),
        (402, 1),
        (313, 2),
        (325, 2),
        (195, 1),
        (107, 4),
        (392, 1),
        (49, 1),
        (271, 1),
        (518, 1),
        (2121, 1),
        (179, 3),
        (32, 2),
        (213, 2),
        (239, 1),
        (866, 1),
        (155, 1),
    ]);
    let path = generate_dist_img(String::from("user_foo"), 47316, 10, 624.91, 2987.0, &data);
    dbg!(path);
}

fn mean_time(data_raw: &HashMap<usize, usize>) -> f64 {
    data_raw
        .iter()
        .fold((0, 0.0), |acc, (time, count)| {
            let n = acc.0;
            let m = acc.1;
            (
                n + count,
                (m * n as f64 + (time * count) as f64) / (n + count) as f64,
            )
        })
        .1
}

#[test]
fn test_mean() {
    assert_eq!(mean_time(&HashMap::from([(1, 1), (2, 1), (3, 1)])), 2.0);
    assert_eq!(mean_time(&HashMap::from([(1, 1), (2, 2), (3, 1)])), 2.0);
    assert_eq!(mean_time(&HashMap::from([(1, 2), (2, 1), (3, 1)])), 1.75);
}

fn std_dev(data_raw: &HashMap<usize, usize>, mean: f64) -> f64 {
    let (sum, count) = data_raw.iter().fold((0.0, 0), |acc, (time, count)| {
        let dev = *time as f64 - mean;
        let d = acc.0 + dev * dev * *count as f64;
        let n = acc.1 + count;
        (d, n)
    });
    dbg!(sum, count);
    (sum / count as f64).sqrt()
}

#[test]
fn test_dev() {
    let exp = 326.93090735172;
    let real = std_dev(
        &HashMap::from([
            (656, 2),
            (549, 1),
            (1, 1),
            (48, 3),
            (49, 1),
            (385, 1),
            (85, 1),
            (984, 1),
        ]),
        319.0,
    );
    dbg!(real);
    assert!((exp - real).abs() < 1e-6);
}

async fn daily_message(ctx: &Context, channel: &ChannelId) {
    let mut interval = time::interval(Duration::from_secs(1));
    let mut ran = false;
    println!("Daily loop starting");
    let typing = channel.start_typing(&ctx.http);
    scan_all(ctx, channel).await.unwrap();
    typing.stop();
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

fn user_time_distrib(all_time: &HashMap<usize, DailyScores>, user: &User) -> HashMap<usize, usize> {
    let mut res = HashMap::new();
    for game in all_time.iter().filter_map(|(_, game)| {
        if game.contains_key(&user) {
            Some(game)
        } else {
            None
        }
    }) {
        *res.entry(game.get(&user).unwrap().secs).or_default() += 1;
    }
    return res;
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
            rank.0.global_name.as_ref().unwrap()
        )
        .unwrap();
    }
    if let Some(rank) = ordered.get(1) {
        writeln!(
            str,
            "2. 🥈 {} {}",
            pp_secs(rank.1.secs),
            rank.0.global_name.as_ref().unwrap()
        )
        .unwrap();
    }
    if let Some(rank) = ordered.get(2) {
        writeln!(
            str,
            "3. 🥉 {} {}",
            pp_secs(rank.1.secs),
            rank.0.global_name.as_ref().unwrap()
        )
        .unwrap();
    }
    for (i, rank) in ordered.iter().enumerate().skip(3) {
        writeln!(
            str,
            "{}.       {} {}",
            i + 1,
            pp_secs(rank.1.secs),
            rank.0.global_name.as_ref().unwrap()
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
            user.global_name.as_ref().unwrap(),
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
        user.id = UserId::new(id);
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
                    secs: 56 + 60 * 10,
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
                    secs: 56 + 60 * 10 + 2 * 3600,
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
                    secs: 56 + 60 * 10,
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
                    secs: 56 + 60 * 10,
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
                    secs: 56 + 60 * 10,
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
                    secs: 24 * 60 * 60 + 1 * 60 + 38,
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

    #[test]
    fn it_distrib() {
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
        day3.insert(make_fake_user(1, "alice"), Score { tries: 2, secs: 14 });
        day3.insert(make_fake_user(2, "bob"), Score { tries: 1, secs: 19 });
        day3.insert(make_fake_user(3, "chary"), Score { tries: 3, secs: 32 });
        leaderboard.insert(1, day1);
        leaderboard.insert(2, day2);
        leaderboard.insert(3, day3);

        let alice = make_fake_user(1, "alice");
        let distrib = user_time_distrib(&leaderboard, &alice);
        println!("{:?}", distrib);
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
        .event_handler(Handler {
            is_daily_running: AtomicBool::new(false),
        })
        .await
        .expect("Err creating client");
    {
        let mut data = client.data.write().await;
        data.insert::<Leaderboard>(leaderboard);
        let conf = Conf {
            sutom_channel: ChannelId::new(sutom_channel_id),
            admin_id: UserId::new(admin_user_id),
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
