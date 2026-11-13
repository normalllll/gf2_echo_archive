use crate::{
    catalog::{Category, Language, scan_resources},
    discovery::validate_directories,
    extraction::{ExtractOptions, extract_source},
    live2d, portrait,
    story::{
        StoryIndex,
        export::{ExportOptions, export},
        load_or_build,
    },
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

pub fn run(arguments: Vec<OsString>) -> Result<()> {
    let command = arguments
        .first()
        .and_then(|arg| arg.to_str())
        .unwrap_or_default();
    if command == "--help" {
        println!(
            "{}",
            tr!(
            "--scan <游戏目录>\n--export <游戏目录> <输出目录> <Character|Story|BGM|Sound|Other> <完整资源名，不含扩展名和语言后缀> <CN|JP|Common> [--overwrite]\n--story-index <游戏目录>\n--story-export <游戏目录> <输出目录> <脚本名|章节目录关键字> [--with-audio] [--by-speaker]
--portrait-index <游戏目录>
--portrait-export <游戏目录> <输出目录> <资源名或关键字> [--jpg] [--overwrite]
--live2d-index <游戏目录>
--live2d-export <游戏目录> <输出目录> <模型名或关键字> [--verify-core] [--overwrite]
--characters <游戏目录>",
            "--scan <game directory>\n--export <game directory> <output directory> <Character|Story|BGM|Sound|Other> <full resource name, without extension or language suffix> <CN|JP|Common> [--overwrite]\n--story-index <game directory>\n--story-export <game directory> <output directory> <script name|chapter directory keyword> [--with-audio] [--by-speaker]
--portrait-index <game directory>
--portrait-export <game directory> <output directory> <resource name or keyword> [--jpg] [--overwrite]
--live2d-index <game directory>
--live2d-export <game directory> <output directory> <model name or keyword> [--verify-core] [--overwrite]
--characters <game directory>"
            )
        );
        println!(
            "
{}
{}",
            crate::ui::free_notice(),
            crate::ui::REPO_URL
        );
        return Ok(());
    }
    let root = PathBuf::from(arguments.get(1).context(tr!(
        "缺少游戏目录；使用 --help 查看用法",
        "Missing game directory; use --help for usage"
    ))?);
    ensure!(
        root.is_dir(),
        tr!("游戏目录不存在", "The game directory does not exist")
    );
    let cancel = Arc::new(AtomicBool::new(false));
    match command {
        "--scan" => {
            ensure!(
                arguments.len() == 2,
                tr!(
                    "--scan 只接受一个游戏目录",
                    "--scan takes one game directory"
                )
            );
            println!(
                "{}",
                serde_json::to_string_pretty(&scan_resources(&root, &cancel))?
            );
        }
        "--export" => {
            ensure!(
                arguments.len() == 6 || (arguments.len() == 7 && arguments[6] == "--overwrite"),
                tr!(
                    "导出参数无效；使用 --help 查看用法",
                    "Invalid export arguments; use --help for usage"
                )
            );
            let output = PathBuf::from(&arguments[2]);
            validate_directories(&root, &output)?;
            let category = Category::ALL
                .into_iter()
                .find(|category| {
                    arguments[3]
                        .to_string_lossy()
                        .eq_ignore_ascii_case(category.folder())
                })
                .context(tr!("资源分类无效", "Invalid resource category"))?;
            let key = arguments[4].to_string_lossy();
            let language = match arguments[5].to_string_lossy().to_ascii_uppercase().as_str() {
                "CN" => Language::Cn,
                "JP" => Language::Jp,
                "COMMON" => Language::Common,
                _ => bail!(tr!(
                    "语言只能是 CN、JP 或 Common",
                    "The language must be CN, JP or Common"
                )),
            };
            let report = scan_resources(&root, &cancel);
            let sources: Vec<_> = report
                .sources
                .into_iter()
                .filter(|source| {
                    source.category == category
                        && source.language == language
                        && source.key.eq_ignore_ascii_case(&key)
                })
                .collect();
            ensure!(
                !sources.is_empty(),
                tr!(
                    "没有匹配的已下载音频包",
                    "No matching downloaded audio pack"
                )
            );
            let options = ExtractOptions {
                output_directory: output,
                sources,
                character_name: None,
                overwrite: arguments.len() == 7,
                cancel,
            };
            for source in &options.sources {
                let result = extract_source(source, &options)?;
                println!(
                    "{}",
                    serde_json::json!({"source": source.path, "output": result.directory, "wav_count": result.wav_count, "reused": result.reused, "decoder_warning_count": result.decoder_warning_count})
                );
            }
        }
        "--story-index" => {
            ensure!(
                arguments.len() == 2,
                tr!(
                    "--story-index 只接受一个游戏目录",
                    "--story-index takes one game directory"
                )
            );
            let index = load_story_index(&root, &cancel)?;
            println!("{}", serde_json::to_string_pretty(&index.summary)?);
        }
        "--story-export" => {
            let flags = arguments.get(4..).unwrap_or_default();
            let with_audio = flags.iter().any(|flag| flag == "--with-audio");
            let by_speaker = flags.iter().any(|flag| flag == "--by-speaker");
            ensure!(
                arguments.len() >= 4
                    && flags
                        .iter()
                        .all(|flag| flag == "--with-audio" || flag == "--by-speaker"),
                tr!(
                    "剧情导出参数无效；使用 --help 查看用法",
                    "Invalid story export arguments; use --help for usage"
                )
            );
            ensure!(
                !by_speaker || with_audio,
                tr!(
                    "--by-speaker 需要同时使用 --with-audio",
                    "--by-speaker requires --with-audio"
                )
            );
            let output = PathBuf::from(&arguments[2]);
            validate_directories(&root, &output)?;
            let index = load_story_index(&root, &cancel)?;
            let selection = index.resolve_selection(&arguments[3].to_string_lossy())?;
            let sources = with_audio.then(|| scan_resources(&root, &cancel).sources);
            let options = ExportOptions {
                output,
                with_audio,
                by_speaker,
                cancel,
            };
            let (summary, _) = export(&index, &selection, &options, sources.as_deref(), &|line| {
                eprintln!("{}", line.text)
            })?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"index": index.summary, "export": summary})
                )?
            );
        }
        "--portrait-index" => {
            ensure!(
                arguments.len() == 2,
                tr!(
                    "--portrait-index 只接受一个游戏目录",
                    "--portrait-index takes one game directory"
                )
            );
            let index = load_portrait_index(&root, &cancel)?;
            println!("{}", serde_json::to_string_pretty(&index.summary)?);
        }
        "--portrait-export" => {
            let flags = arguments.get(4..).unwrap_or_default();
            ensure!(
                arguments.len() >= 4
                    && flags
                        .iter()
                        .all(|flag| flag == "--jpg" || flag == "--overwrite"),
                tr!(
                    "立绘导出参数无效；使用 --help 查看用法",
                    "Invalid portrait export arguments; use --help for usage"
                )
            );
            let output = PathBuf::from(&arguments[2]);
            validate_directories(&root, &output)?;
            let index = load_portrait_index(&root, &cancel)?;
            let keyword = arguments[3].to_string_lossy();
            let entries = index.select(&keyword);
            ensure!(
                !entries.is_empty(),
                tr!(
                    "没有名称包含 {keyword} 的立绘",
                    "No portrait name contains {keyword}",
                    keyword = keyword
                )
            );
            let options = portrait::ExportOptions {
                output,
                jpg: flags.iter().any(|flag| flag == "--jpg"),
                overwrite: flags.iter().any(|flag| flag == "--overwrite"),
            };
            let summary = portrait::export(&index, &entries, &options, &cancel, &|_, _, _| {})?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"index": index.summary, "export": summary})
                )?
            );
        }
        "--live2d-index" => {
            ensure!(
                arguments.len() == 2,
                tr!(
                    "--live2d-index 只接受一个游戏目录",
                    "--live2d-index takes one game directory"
                )
            );
            let index = load_live2d_index(&root, &cancel)?;
            println!("{}", serde_json::to_string_pretty(&index.summary)?);
        }
        "--live2d-export" => {
            let flags = arguments.get(4..).unwrap_or_default();
            ensure!(
                arguments.len() >= 4
                    && flags
                        .iter()
                        .all(|flag| flag == "--verify-core" || flag == "--overwrite"),
                tr!(
                    "Live2D 导出参数无效；使用 --help 查看用法",
                    "Invalid Live2D export arguments; use --help for usage"
                )
            );
            let output = PathBuf::from(&arguments[2]);
            validate_directories(&root, &output)?;
            let index = load_live2d_index(&root, &cancel)?;
            let keyword = arguments[3].to_string_lossy();
            let entries = index.select(&keyword);
            ensure!(
                !entries.is_empty(),
                tr!(
                    "没有名称包含 {keyword} 的 Live2D 模型",
                    "No Live2D model name contains {keyword}",
                    keyword = keyword
                )
            );
            let options = live2d::ExportOptions {
                output,
                overwrite: flags.iter().any(|flag| flag == "--overwrite"),
                verify_core: flags.iter().any(|flag| flag == "--verify-core"),
            };
            let summary = live2d::export(&index, &entries, &options, &cancel, &|_, _, _| {})?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"index": index.summary, "export": summary})
                )?
            );
        }
        "--characters" => {
            ensure!(
                arguments.len() == 2,
                tr!(
                    "--characters 只接受一个游戏目录",
                    "--characters takes one game directory"
                )
            );
            let report = crate::roster::load::report(&root, None, &cancel)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        _ => bail!(tr!(
            "未知命令；使用 --help 查看用法",
            "Unknown command; use --help for usage"
        )),
    }
    Ok(())
}

fn load_live2d_index(root: &Path, cancel: &AtomicBool) -> Result<live2d::Live2dIndex> {
    live2d::load_or_build(root, true, cancel, &|_| {})?
        .context(tr!("索引已取消", "Indexing was cancelled"))
}

fn load_portrait_index(root: &Path, cancel: &AtomicBool) -> Result<portrait::PortraitIndex> {
    portrait::load_or_build(root, true, cancel, &|_| {})?
        .context(tr!("索引已取消", "Indexing was cancelled"))
}

fn load_story_index(root: &Path, cancel: &AtomicBool) -> Result<StoryIndex> {
    load_or_build(root, true, cancel, &|_| {})?.context(tr!("索引已取消", "Indexing was cancelled"))
}
