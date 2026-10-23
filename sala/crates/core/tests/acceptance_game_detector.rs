//! Testes de aceite do detector de jogo (ex-ocultos do par T-010; `adv` = adversariais).
use sala_core::config::GameModeConfig;
use sala_core::game_detector::*;

fn s(path: &str, covers: bool) -> ForegroundSample {
    ForegroundSample { exe_path: path.into(), covers_monitor: covers }
}
fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}
fn rules() -> GameRules {
    GameRules::new(&strs(&["valorant.exe", "cs2.exe"]), &strs(&["explorer.exe", "chrome.exe", "vlc.exe"]))
}
fn det() -> GameDetector {
    GameDetector::new(&GameModeConfig::default())
}
const GAME: &str = r"C:\Program Files (x86)\Steam\steamapps\common\Counter-Strike Global Offensive\game\bin\win64\cs2.exe";
const DESK: &str = r"C:\Windows\explorer.exe";
fn pos() -> ForegroundSample {
    s(GAME, true)
}
fn neg() -> ForegroundSample {
    s(DESK, false)
}

#[test]
fn h01_constants_and_initial_state() {
    assert_eq!(Hysteresis::ENTER_AFTER, 2);
    assert_eq!(Hysteresis::EXIT_AFTER, 3);
    assert_eq!(POLL_INTERVAL, std::time::Duration::from_secs(2));
    let h = Hysteresis::new();
    assert!(!h.in_game());
    let d = det();
    assert!(!d.in_game());
    assert_eq!(d.origin(), Origin::Auto);
}

#[test]
fn h02_games_list_matches_full_path_case_insensitive() {
    let r = rules();
    assert!(r.is_game(Some(&s(r"C:\Riot Games\VALORANT\live\VALORANT.EXE", false))));
    assert!(r.is_game(Some(&s(GAME, false))));
    assert!(r.is_game(Some(&s("cs2.exe", false))), "nome sem diretório");
    let upper = GameRules::new(&strs(&["Game.EXE"]), &[]);
    assert!(upper.is_game(Some(&s(r"d:\x\game.exe", false))), "lista da config também é case-insensitive");
}

#[test]
fn h03_fullscreen_rule_and_exclude() {
    let r = rules();
    assert!(r.is_game(Some(&s(r"D:\jogos\indie\IndieGame.exe", true))), "tela cheia fora da exclusão");
    assert!(!r.is_game(Some(&s(r"D:\jogos\indie\IndieGame.exe", false))), "janela comum fora da lista");
    assert!(!r.is_game(Some(&s(r"C:\Program Files\Google\Chrome\Application\CHROME.EXE", true))), "navegador em tela cheia");
    assert!(!r.is_game(Some(&s(r"C:\Program Files\VideoLAN\VLC\vlc.exe", true))));
    assert!(!r.is_game(None), "sem janela em primeiro plano = negativo");
}

#[test]
fn h04_adv_match_is_whole_file_name_not_suffix_or_substring() {
    let r = rules();
    assert!(!r.is_game(Some(&s(r"C:\tools\notcs2.exe", false))), "sufixo não é o mesmo executável");
    assert!(!r.is_game(Some(&s(r"C:\Riot Games\VALORANT\live\VALORANT-Win64-Shipping.exe", false))));
    assert!(!r.is_game(Some(&s(r"C:\cs2.exe\launcher.exe", false))), "diretório com o nome do jogo");
    // tela cheia + exclusão também compara o nome inteiro
    assert!(r.is_game(Some(&s(r"C:\apps\myexplorer.exe", true))), "myexplorer.exe não está na exclusão");
}

#[test]
fn h05_games_list_wins_over_exclude() {
    let r = GameRules::new(&strs(&["vlc.exe"]), &strs(&["vlc.exe"]));
    assert!(r.is_game(Some(&s(r"C:\VLC\vlc.exe", false))), "jogo listado é positivo mesmo excluído e sem tela cheia");
}

#[test]
fn h06_enter_after_two_consecutive_positives_once() {
    let mut h = Hysteresis::new();
    assert_eq!(h.observe(true), None);
    assert_eq!(h.observe(true), Some(Transition::Enter));
    assert!(h.in_game());
    assert_eq!(h.observe(true), None, "Enter só uma vez");
    assert_eq!(h.observe(true), None);
}

#[test]
fn h07_exit_after_three_consecutive_negatives_once() {
    let mut h = Hysteresis::new();
    h.observe(true);
    h.observe(true);
    assert_eq!(h.observe(false), None);
    assert_eq!(h.observe(false), None);
    assert_eq!(h.observe(false), Some(Transition::Exit));
    assert!(!h.in_game());
    assert_eq!(h.observe(false), None, "Exit só uma vez");
}

#[test]
fn h08_positive_resets_negative_streak_and_vice_versa() {
    let mut h = Hysteresis::new();
    assert_eq!(h.observe(true), None);
    assert_eq!(h.observe(false), None);
    assert_eq!(h.observe(true), None, "positivo isolado após negativo não entra");
    assert_eq!(h.observe(true), Some(Transition::Enter));
    for _ in 0..2 {
        assert_eq!(h.observe(false), None);
    }
    assert_eq!(h.observe(true), None);
    assert_eq!(h.observe(false), None);
    assert_eq!(h.observe(false), None);
    assert!(h.in_game(), "N,N,P,N,N não sai");
    assert_eq!(h.observe(false), Some(Transition::Exit));
}

#[test]
fn h09_adv_alternation_never_changes_state() {
    let mut h = Hysteresis::new();
    for i in 0..50 {
        assert_eq!(h.observe(i % 2 == 0), None, "leitura {i}");
    }
    assert!(!h.in_game());
    h.reset(true);
    let mut p = false;
    for i in 0..50 {
        // N,N,P,N,N,P...: nunca 3 negativos seguidos
        let positive = i % 3 == 2;
        p |= h.observe(positive).is_some();
    }
    assert!(!p && h.in_game());
}

#[test]
fn h10_reset_clears_streaks() {
    let mut h = Hysteresis::new();
    h.observe(true);
    h.reset(false);
    assert_eq!(h.observe(true), None, "contador zerado no reset");
    assert_eq!(h.observe(true), Some(Transition::Enter));
    h.observe(false);
    h.observe(false);
    h.reset(true);
    assert_eq!(h.observe(false), None);
    assert_eq!(h.observe(false), None);
    assert_eq!(h.observe(false), Some(Transition::Exit));
}

#[test]
fn h11_detector_auto_enter_and_exit() {
    let mut d = det();
    assert_eq!(d.tick(Some(&pos())), None);
    assert_eq!(d.tick(Some(&pos())), Some(Transition::Enter));
    assert!(d.in_game());
    assert_eq!(d.origin(), Origin::Auto);
    assert_eq!(d.tick(None), None);
    assert_eq!(d.tick(Some(&neg())), None);
    assert_eq!(d.tick(Some(&neg())), Some(Transition::Exit));
    assert!(!d.in_game());
}

#[test]
fn h12_auto_detect_off_never_transitions_but_manual_works() {
    let cfg = GameModeConfig { auto_detect: false, ..GameModeConfig::default() };
    let mut d = GameDetector::new(&cfg);
    for _ in 0..5 {
        assert_eq!(d.tick(Some(&pos())), None);
    }
    assert_eq!(d.set_manual(true), Some(Transition::Enter));
    assert!(d.in_game());
    assert_eq!(d.set_manual(false), Some(Transition::Exit));
}

#[test]
fn h13_adv_manual_enter_is_not_exited_by_detector() {
    let mut d = det();
    assert_eq!(d.set_manual(true), Some(Transition::Enter));
    assert_eq!(d.origin(), Origin::Manual);
    for i in 0..10 {
        assert_eq!(d.tick(Some(&neg())), None, "tick {i}");
    }
    assert!(d.in_game());
    assert_eq!(d.set_manual(true), None, "já está no modo jogo");
    assert_eq!(d.set_manual(false), Some(Transition::Exit));
    assert_eq!(d.set_manual(false), None);
}

#[test]
fn h14_adv_manual_exit_suppresses_reentry_until_a_negative() {
    let mut d = det();
    d.tick(Some(&pos()));
    assert_eq!(d.tick(Some(&pos())), Some(Transition::Enter));
    assert_eq!(d.set_manual(false), Some(Transition::Exit));
    for i in 0..6 {
        assert_eq!(d.tick(Some(&pos())), None, "jogo ainda em foco não reentra (tick {i})");
    }
    assert_eq!(d.tick(Some(&neg())), None);
    assert_eq!(d.tick(Some(&pos())), None, "só conta leituras após o negativo");
    assert_eq!(d.tick(Some(&pos())), Some(Transition::Enter));
    assert_eq!(d.origin(), Origin::Auto);
}

#[test]
fn h15_adv_manual_on_while_auto_in_game_takes_ownership() {
    let mut d = det();
    d.tick(Some(&pos()));
    d.tick(Some(&pos()));
    assert_eq!(d.set_manual(true), None);
    assert_eq!(d.origin(), Origin::Manual);
    for _ in 0..5 {
        assert_eq!(d.tick(None), None);
    }
    assert!(d.in_game());
}

#[test]
fn h16_adv_set_config_clears_streaks_and_applies_lists() {
    let mut d = det();
    d.tick(Some(&pos()));
    d.set_config(&GameModeConfig::default());
    assert_eq!(d.tick(Some(&pos())), None, "contador zerado");
    assert_eq!(d.tick(Some(&pos())), Some(Transition::Enter));
    d.tick(Some(&neg()));
    d.tick(Some(&neg()));
    d.set_config(&GameModeConfig::default());
    assert!(d.in_game(), "set_config mantém o estado");
    assert_eq!(d.tick(Some(&neg())), None);
    assert_eq!(d.tick(Some(&neg())), None);
    assert_eq!(d.tick(Some(&neg())), Some(Transition::Exit));

    let only_x = GameModeConfig { games: strs(&["x.exe"]), exclude: vec![], ..GameModeConfig::default() };
    d.set_config(&only_x);
    let x = s(r"C:\x.exe", false);
    assert_eq!(d.tick(Some(&x)), None);
    assert_eq!(d.tick(Some(&x)), Some(Transition::Enter), "nova lista aplicada");
    let off = GameModeConfig { auto_detect: false, ..only_x };
    d.set_config(&off);
    for _ in 0..5 {
        assert_eq!(d.tick(None), None, "auto_detect desligado");
    }
}
