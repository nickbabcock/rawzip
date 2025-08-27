use quickcheck_macros::quickcheck;
use rawzip::extra_fields::ExtraFieldId;
use rawzip::time::{LocalDateTime, UtcDateTime, ZipDateTimeKind};
use rawzip::{Error, ErrorKind, ZipArchive};
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;

mod extra_data_zip_tests;
mod extra_fields_test;
mod false_signature_tests;
mod modification_time_tests;
mod permission_tests;
mod utf8_tests;
mod zip64_tests;

macro_rules! zip_test_case {
    ($name:expr, $case:expr) => {
        paste::paste! {
            #[test]
            fn [<test_ $name _reader >]() {
                run_zip_test_case_reader(&$case);
            }

            #[test]
            fn [<test_ $name _slice >]() {
                run_zip_test_case_slice(&$case);
            }
        }
    };
}

#[derive(Debug, Default)]
struct ZipTestCase {
    name: &'static str,
    comment: Option<&'static [u8]>,
    files: Vec<ZipTestFileEntry>,
    expected_error_kind: Option<ErrorKind>,
}

#[derive(Debug)]
struct ZipTestFileEntry {
    name: &'static str,
    expected_content: ExpectedContent,
    expected_datetime: Option<ZipDateTimeKind>,
    expected_mode: Option<u32>,
}

#[derive(Debug)]
enum ExpectedContent {
    Content(Vec<u8>),
    File(&'static str),
    // Size(u64),
}

zip_test_case!(
    "test",
    ZipTestCase {
        name: "test.zip",
        comment: Some(b"This is a zipfile comment."),
        files: vec![
            ZipTestFileEntry {
                name: "test.txt",
                expected_content: ExpectedContent::Content(b"This is a test text file.\n".to_vec(),),
                expected_datetime: Some(ZipDateTimeKind::Utc(
                    UtcDateTime::from_components(2010, 9, 5, 2, 12, 1, 0).unwrap()
                )), // 2010-09-05 02:12:01 UTC
                expected_mode: Some(0o100644), // Regular file with 644 permissions
            },
            ZipTestFileEntry {
                name: "gophercolor16x16.png",
                expected_content: ExpectedContent::File("gophercolor16x16.png"),
                expected_datetime: Some(ZipDateTimeKind::Utc(
                    UtcDateTime::from_components(2010, 9, 5, 5, 52, 58, 0).unwrap()
                )), // 2010-09-05 05:52:58 UTC
                expected_mode: Some(0o100644), // Regular file with 644 permissions
            },
        ],
        ..Default::default()
    }
);

zip_test_case!(
    "readme_notzip",
    ZipTestCase {
        name: "readme.notzip",
        expected_error_kind: Some(ErrorKind::MissingEndOfCentralDirectory),
        ..Default::default()
    }
);

zip_test_case!(
    "test_trailing_junk",
    ZipTestCase {
        name: "test-trailing-junk.zip",
        comment: Some(b"This is a zipfile comment."),
        files: vec![
            ZipTestFileEntry {
                name: "test.txt",
                expected_content: ExpectedContent::Content(b"This is a test text file.\n".to_vec(),),
                expected_datetime: Some(ZipDateTimeKind::Utc(
                    UtcDateTime::from_components(2010, 9, 5, 2, 12, 1, 0).unwrap()
                )), // 2010-09-05 02:12:01 UTC
                expected_mode: Some(0o100644), // Regular file with 644 permissions
            },
            ZipTestFileEntry {
                name: "gophercolor16x16.png",
                expected_content: ExpectedContent::File("gophercolor16x16.png"),
                expected_datetime: Some(ZipDateTimeKind::Utc(
                    UtcDateTime::from_components(2010, 9, 5, 5, 52, 58, 0).unwrap()
                )), // 2010-09-05 05:52:58 UTC
                expected_mode: Some(0o100644), // Regular file with 644 permissions
            },
        ],
        ..Default::default()
    }
);

zip_test_case!(
    "test_prefix",
    ZipTestCase {
        name: "test-prefix.zip",
        comment: Some(b"This is a zipfile comment."),
        files: vec![
            ZipTestFileEntry {
                name: "test.txt",
                expected_content: ExpectedContent::Content(b"This is a test text file.\n".to_vec(),),
                expected_datetime: Some(ZipDateTimeKind::Utc(
                    UtcDateTime::from_components(2010, 9, 5, 2, 12, 1, 0).unwrap()
                )), // 2010-09-05 02:12:01 UTC
                expected_mode: Some(0o100644), // Regular file with 644 permissions
            },
            ZipTestFileEntry {
                name: "gophercolor16x16.png",
                expected_content: ExpectedContent::File("gophercolor16x16.png"),
                expected_datetime: Some(ZipDateTimeKind::Utc(
                    UtcDateTime::from_components(2010, 9, 5, 5, 52, 58, 0).unwrap()
                )), // 2010-09-05 05:52:58 UTC
                expected_mode: Some(0o100644), // Regular file with 644 permissions
            },
        ],
        ..Default::default()
    }
);

zip_test_case!(
    "symlink",
    ZipTestCase {
        name: "symlink.zip",
        files: vec![ZipTestFileEntry {
            name: "symlink",
            expected_content: ExpectedContent::Content(b"../target".to_vec()),
            expected_datetime: Some(ZipDateTimeKind::Utc(
                UtcDateTime::from_components(2012, 2, 3, 21, 56, 48, 0).unwrap()
            )), // 2012-02-03 21:56:48 (UTC from archive)
            expected_mode: Some(0o120777), // Symlink with 777 permissions
        }],
        ..Default::default()
    }
);

zip_test_case!(
    "readme",
    ZipTestCase {
        name: "readme.zip",
        ..Default::default()
    }
);

zip_test_case!(
    "winxp",
    ZipTestCase {
        // created in windows XP file manager.
        name: "winxp.zip",
        files: vec![
            ZipTestFileEntry {
                name: "hello",
                expected_content: ExpectedContent::Content(b"world \r\n".to_vec()),
                expected_datetime: Some(ZipDateTimeKind::Local(
                    LocalDateTime::from_components(2011, 12, 8, 10, 4, 24, 0).unwrap()
                )),
                expected_mode: Some(0o100666), // Regular file with 666 permissions (Windows)
            },
            ZipTestFileEntry {
                name: "dir/bar",
                expected_content: ExpectedContent::Content(b"foo \r\n".to_vec()),
                expected_datetime: Some(ZipDateTimeKind::Local(
                    LocalDateTime::from_components(2011, 12, 8, 10, 4, 50, 0).unwrap()
                )),
                expected_mode: Some(0o100666), // Regular file with 666 permissions (Windows)
            },
            ZipTestFileEntry {
                name: "dir/empty/",
                expected_content: ExpectedContent::Content(b"".to_vec()),
                expected_datetime: Some(ZipDateTimeKind::Local(
                    LocalDateTime::from_components(2011, 12, 8, 10, 8, 6, 0).unwrap()
                )),
                expected_mode: Some(0o040777), // Directory with 777 permissions (Windows)
            },
            ZipTestFileEntry {
                name: "readonly",
                expected_content: ExpectedContent::Content(b"important \r\n".to_vec()),
                expected_datetime: Some(ZipDateTimeKind::Local(
                    LocalDateTime::from_components(2011, 12, 8, 10, 6, 8, 0).unwrap()
                )),
                expected_mode: Some(0o100444), // Read-only file (Windows)
            },
        ],
        ..Default::default()
    }
);

zip_test_case!(
    "unix",
    ZipTestCase {
        // created by Zip 3.0 under Linux
        name: "unix.zip",
        files: vec![
            ZipTestFileEntry {
                name: "hello",
                expected_content: ExpectedContent::Content(b"world \r\n".to_vec()),
                expected_datetime: Some(ZipDateTimeKind::Utc(
                    UtcDateTime::from_components(2011, 12, 8, 10, 4, 24, 0).unwrap()
                )), // 2011-12-08 10:04:24 UTC (but stored as local time)
                expected_mode: Some(0o100666), // Regular file with 666 permissions (Unix)
            },
            ZipTestFileEntry {
                name: "dir/bar",
                expected_content: ExpectedContent::Content(b"foo \r\n".to_vec()),
                expected_datetime: Some(ZipDateTimeKind::Utc(
                    UtcDateTime::from_components(2011, 12, 8, 10, 4, 50, 0).unwrap()
                )), // 2011-12-08 10:04:50 UTC (but stored as local time)
                expected_mode: Some(0o100666), // Regular file with 666 permissions (Unix)
            },
            ZipTestFileEntry {
                name: "dir/empty/",
                expected_content: ExpectedContent::Content(b"".to_vec()),
                expected_datetime: Some(ZipDateTimeKind::Utc(
                    UtcDateTime::from_components(2011, 12, 8, 10, 8, 6, 0).unwrap()
                )), // 2011-12-08 10:08:06 UTC (but stored as local time)
                expected_mode: Some(0o040777), // Directory with 777 permissions (Unix)
            },
            ZipTestFileEntry {
                name: "readonly",
                expected_content: ExpectedContent::Content(b"important \r\n".to_vec()),
                expected_datetime: Some(ZipDateTimeKind::Utc(
                    UtcDateTime::from_components(2011, 12, 8, 10, 6, 8, 0).unwrap()
                )), // 2011-12-08 10:06:08 UTC (but stored as local time)
                expected_mode: Some(0o100444), // Read-only file (Unix)
            },
        ],
        ..Default::default()
    }
);

zip_test_case!(
    "go_with_datadesc_sig",
    ZipTestCase {
        // created by Go, after we wrote the "optional" data
        // descriptor signatures (which are required by macOS)
        name: "go-with-datadesc-sig.zip",
        files: vec![
            ZipTestFileEntry {
                name: "foo.txt",
                expected_content: ExpectedContent::Content(b"foo\n".to_vec()),
                expected_datetime: Some(ZipDateTimeKind::Local(
                    LocalDateTime::from_components(1980, 1, 1, 0, 0, 0, 0).unwrap()
                )), // DOS timestamp 0x0000 0x0000 normalized to 1980-01-01 00:00:00
                expected_mode: Some(0o100666), // Regular file with 666 permissions
            },
            ZipTestFileEntry {
                name: "bar.txt",
                expected_content: ExpectedContent::Content(b"bar\n".to_vec()),
                expected_datetime: Some(ZipDateTimeKind::Local(
                    LocalDateTime::from_components(1980, 1, 1, 0, 0, 0, 0).unwrap()
                )), // DOS timestamp 0x0000 0x0000 normalized to 1980-01-01 00:00:00
                expected_mode: Some(0o100666), // Regular file with 666 permissions
            },
        ],
        ..Default::default()
    }
);

zip_test_case!(
    "crc32_not_streamed",
    ZipTestCase {
        name: "crc32-not-streamed.zip",
        files: vec![
            ZipTestFileEntry {
                name: "foo.txt",
                expected_content: ExpectedContent::Content(b"foo\n".to_vec()),
                expected_datetime: Some(ZipDateTimeKind::Utc(
                    UtcDateTime::from_components(2012, 3, 9, 0, 59, 10, 0).unwrap()
                )), // 2012-03-09 00:59:10 (UTC from archive)
                expected_mode: Some(0o100644), // Regular file with 644 permissions
            },
            ZipTestFileEntry {
                name: "bar.txt",
                expected_content: ExpectedContent::Content(b"bar\n".to_vec()),
                expected_datetime: Some(ZipDateTimeKind::Utc(
                    UtcDateTime::from_components(2012, 3, 9, 0, 59, 12, 0).unwrap()
                )), // 2012-03-09 00:59:12 (UTC from archive)
                expected_mode: Some(0o100644), // Regular file with 644 permissions
            },
        ],
        ..Default::default()
    }
);

zip_test_case!(
    "zip64_2",
    ZipTestCase {
        name: "zip64-2.zip",
        files: vec![ZipTestFileEntry {
            name: "README",
            expected_content: ExpectedContent::Content(
                b"This small file is in ZIP64 format.\n".to_vec(),
            ),
            expected_datetime: Some(ZipDateTimeKind::Utc(
                UtcDateTime::from_components(2012, 8, 10, 18, 33, 32, 0).unwrap()
            )), // 2012-08-10 18:33:32 (UTC from archive)
            expected_mode: Some(0o100644), // Regular file with 644 permissions
        }],
        ..Default::default()
    }
);

zip_test_case!(
    "time_7zip",
    ZipTestCase {
        name: "time-7zip.zip",
        files: vec![ZipTestFileEntry {
            name: "test.txt",
            expected_content: ExpectedContent::Content(vec![]),
            expected_datetime: Some(ZipDateTimeKind::Utc(
                UtcDateTime::from_components(2017, 11, 1, 4, 11, 57, 244817900).unwrap()
            )), // 2017-10-31 21:11:57.244817900 (-7 hours) = 2017-11-01 04:11:57.244817900 UTC
            expected_mode: Some(0o100666), // Regular file with 666 permissions
        }],
        ..Default::default()
    }
);

zip_test_case!(
    "time_infozip",
    ZipTestCase {
        name: "time-infozip.zip",
        files: vec![ZipTestFileEntry {
            name: "test.txt",
            expected_content: ExpectedContent::Content(vec![]),
            expected_datetime: Some(ZipDateTimeKind::Utc(
                UtcDateTime::from_components(2017, 11, 1, 4, 11, 57, 0).unwrap()
            )), // 2017-10-31 21:11:57.000 (-7 hours) = 2017-11-01 04:11:57.000 UTC
            expected_mode: Some(0o100644), // Regular file with 644 permissions
        }],
        ..Default::default()
    }
);

zip_test_case!(
    "time_osx",
    ZipTestCase {
        name: "time-osx.zip",
        files: vec![ZipTestFileEntry {
            name: "test.txt",
            expected_content: ExpectedContent::Content(vec![]),
            expected_datetime: Some(ZipDateTimeKind::Utc(
                UtcDateTime::from_components(2017, 11, 1, 4, 11, 57, 0).unwrap()
            )), // 2017-10-31 21:11:57.000 (-7 hours) = 2017-11-01 04:11:57.000 UTC
            expected_mode: Some(0o100644), // Regular file with 644 permissions
        }],
        ..Default::default()
    }
);

zip_test_case!(
    "time_win7",
    ZipTestCase {
        name: "time-win7.zip",
        files: vec![ZipTestFileEntry {
            name: "test.txt",
            expected_content: ExpectedContent::Content(vec![]),
            expected_datetime: Some(ZipDateTimeKind::Local(
                LocalDateTime::from_components(2017, 10, 31, 21, 11, 58, 0).unwrap()
            )), // 2017-10-31 21:11:58.000 (DOS local time)
            expected_mode: Some(0o100666), // Regular file with 666 permissions
        }],
        ..Default::default()
    }
);

zip_test_case!(
    "time_winrar",
    ZipTestCase {
        name: "time-winrar.zip",
        files: vec![ZipTestFileEntry {
            name: "test.txt",
            expected_content: ExpectedContent::Content(vec![]),
            expected_datetime: Some(ZipDateTimeKind::Utc(
                UtcDateTime::from_components(2017, 11, 1, 4, 11, 57, 244817900).unwrap()
            )), // 2017-10-31 21:11:57.244817900 (-7 hours) = 2017-11-01 04:11:57.244817900 UTC
            expected_mode: Some(0o100666), // Regular file with 666 permissions
        }],
        ..Default::default()
    }
);

zip_test_case!(
    "time_winzip",
    ZipTestCase {
        name: "time-winzip.zip",
        files: vec![ZipTestFileEntry {
            name: "test.txt",
            expected_content: ExpectedContent::Content(vec![]),
            expected_datetime: Some(ZipDateTimeKind::Utc(
                UtcDateTime::from_components(2017, 11, 1, 4, 11, 57, 244000000).unwrap()
            )), // 2017-10-31 21:11:57.244000000 (-7 hours) = 2017-11-01 04:11:57.244000000 UTC
            expected_mode: Some(0o100666), // Regular file with 666 permissions
        }],
        ..Default::default()
    }
);

zip_test_case!(
    "time_go",
    ZipTestCase {
        name: "time-go.zip",
        files: vec![ZipTestFileEntry {
            name: "test.txt",
            expected_content: ExpectedContent::Content(vec![]),
            expected_datetime: Some(ZipDateTimeKind::Utc(
                UtcDateTime::from_components(2017, 11, 1, 4, 11, 57, 0).unwrap()
            )), // 2017-10-31 21:11:57.000 (-7 hours) = 2017-11-01 04:11:57.000 UTC
            expected_mode: Some(0o100666), // Regular file with 666 permissions
        }],
        ..Default::default()
    }
);

zip_test_case!(
    "badbase",
    ZipTestCase {
        name: "test-badbase.zip",
        comment: Some(b"This is a zipfile comment."),
        files: vec![
            ZipTestFileEntry {
                name: "test.txt",
                expected_content: ExpectedContent::Content(b"This is a test text file.\n".to_vec(),),
                expected_datetime: Some(ZipDateTimeKind::Utc(
                    UtcDateTime::from_components(2010, 9, 5, 2, 12, 1, 0).unwrap()
                )), // 2010-09-05 02:12:01 UTC
                expected_mode: Some(0o100644), // Regular file with 644 permissions
            },
            ZipTestFileEntry {
                name: "gophercolor16x16.png",
                expected_content: ExpectedContent::File("gophercolor16x16.png"),
                expected_datetime: Some(ZipDateTimeKind::Utc(
                    UtcDateTime::from_components(2010, 9, 5, 5, 52, 58, 0).unwrap()
                )), // 2010-09-05 05:52:58 UTC
                expected_mode: Some(0o100644), // Regular file with 644 permissions
            },
        ],
        ..Default::default()
    }
);

zip_test_case!(
    "baddirsz",
    ZipTestCase {
        name: "test-baddirsz.zip",
        comment: Some(b"This is a zipfile comment."),
        files: vec![
            ZipTestFileEntry {
                name: "test.txt",
                expected_content: ExpectedContent::Content(b"This is a test text file.\n".to_vec(),),
                expected_datetime: Some(ZipDateTimeKind::Utc(
                    UtcDateTime::from_components(2010, 9, 5, 2, 12, 1, 0).unwrap()
                )), // 2010-09-05 02:12:01 UTC
                expected_mode: Some(0o100644), // Regular file with 644 permissions
            },
            ZipTestFileEntry {
                name: "gophercolor16x16.png",
                expected_content: ExpectedContent::File("gophercolor16x16.png"),
                expected_datetime: Some(ZipDateTimeKind::Utc(
                    UtcDateTime::from_components(2010, 9, 5, 5, 52, 58, 0).unwrap()
                )), // 2010-09-05 05:52:58 UTC
                expected_mode: Some(0o100644), // Regular file with 644 permissions
            },
        ],
        ..Default::default()
    }
);

fn process_archive_files<R: rawzip::ReaderAt>(
    archive: &rawzip::ZipArchive<R>,
    case: &ZipTestCase,
    buf: &mut [u8],
) -> Result<(), Error> {
    if let Some(expected_comment_bytes) = case.comment {
        let mut comment_reader = archive.comment();
        let comment_len = comment_reader.remaining() as usize;
        let mut comment_buffer = vec![0u8; comment_len];
        comment_reader.read_exact(&mut comment_buffer).unwrap();
        assert_eq!(
            comment_buffer.as_slice(),
            expected_comment_bytes,
            "Comment mismatch for {}",
            case.name
        );
    }

    let mut actual_files_found = 0;

    for expected_file in &case.files {
        let mut found_file = false;
        let mut entries_for_current_expected_file = archive.entries(buf);
        loop {
            match entries_for_current_expected_file.next_entry() {
                Ok(Some(entry)) => {
                    if entry.file_path().try_normalize().unwrap().as_ref() == expected_file.name {
                        actual_files_found += 1;
                        found_file = true;

                        if let Some(expected_dt) = &expected_file.expected_datetime {
                            let actual_dt = entry.last_modified();
                            assert_eq!(
                                &actual_dt, expected_dt,
                                "Datetime mismatch for file {}: expected {}, got {}",
                                expected_file.name, expected_dt, actual_dt
                            );
                        }

                        if let Some(expected_mode) = expected_file.expected_mode {
                            let actual_mode = entry.mode().value();
                            assert_eq!(
                                actual_mode, expected_mode,
                                "Mode mismatch for file {}: expected 0o{:o}, got 0o{:o}",
                                expected_file.name, expected_mode, actual_mode
                            );
                        }

                        let position = entry.wayfinder();
                        let ent = archive.get_entry(position)?;

                        let mut data = Vec::new();
                        match entry.compression_method() {
                            rawzip::CompressionMethod::Deflate => {
                                let inflater = flate2::read::DeflateDecoder::new(ent.reader());
                                let mut verifier = ent.verifying_reader(inflater);
                                std::io::copy(&mut verifier, &mut Cursor::new(&mut data)).unwrap();
                            }
                            rawzip::CompressionMethod::Store => {
                                let mut verifier = ent.verifying_reader(ent.reader());
                                std::io::copy(&mut verifier, &mut Cursor::new(&mut data)).unwrap();
                            }
                            _ => todo!(
                                "Compression method not yet handled: {:?}",
                                entry.compression_method()
                            ),
                        }

                        match &expected_file.expected_content {
                            ExpectedContent::Content(expected_bytes) => {
                                assert_eq!(
                                    &data, expected_bytes,
                                    "Content mismatch for file {} in {}",
                                    expected_file.name, case.name
                                );
                            }
                            ExpectedContent::File(content_file_name) => {
                                let content_path = Path::new("assets").join(content_file_name);
                                let expected_bytes = std::fs::read(content_path).unwrap();
                                assert_eq!(
                                    &data, &expected_bytes,
                                    "Content mismatch for file {} (from {}) in {}",
                                    expected_file.name, content_file_name, case.name
                                );
                            }
                        }
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => panic!("Error iterating entries in {}: {:?}", case.name, e),
            }
        }
        if !found_file {
            panic!(
                "Expected file {} not found in archive {}",
                expected_file.name, case.name
            );
        }
    }
    assert_eq!(
        actual_files_found,
        case.files.len(),
        "File count mismatch for {}. Expected {}, found {}",
        case.name,
        case.files.len(),
        actual_files_found
    );

    Ok(())
}

fn process_slice_archive_files(
    archive: &rawzip::ZipSliceArchive<&[u8]>,
    case: &ZipTestCase,
) -> Result<(), Error> {
    if let Some(expected_comment_bytes) = case.comment {
        assert_eq!(
            archive.comment().as_bytes(),
            expected_comment_bytes,
            "Comment mismatch for {}",
            case.name
        );
    }

    let mut actual_files_found = 0;

    for expected_file in &case.files {
        let mut found_file = false;
        let mut entries_for_current_expected_file = archive.entries();
        loop {
            match entries_for_current_expected_file.next_entry() {
                Ok(Some(entry)) => {
                    if entry.file_path().try_normalize().unwrap().as_ref() == expected_file.name {
                        actual_files_found += 1;
                        found_file = true;

                        if let Some(expected_dt) = &expected_file.expected_datetime {
                            let actual_dt = entry.last_modified();
                            assert_eq!(
                                &actual_dt, expected_dt,
                                "Datetime mismatch for file {}: expected {}, got {}",
                                expected_file.name, expected_dt, actual_dt
                            );
                        }

                        if let Some(expected_mode) = expected_file.expected_mode {
                            let actual_mode = entry.mode().value();
                            assert_eq!(
                                actual_mode, expected_mode,
                                "Mode mismatch for file {}: expected 0o{:o}, got 0o{:o}",
                                expected_file.name, expected_mode, actual_mode
                            );
                        }

                        let position = entry.wayfinder();

                        let ent = archive.get_entry(position)?;

                        let mut data = Vec::new();
                        match entry.compression_method() {
                            rawzip::CompressionMethod::Deflate => {
                                let inflater = flate2::read::DeflateDecoder::new(ent.data());
                                let mut verifier = ent.verifying_reader(inflater);
                                std::io::copy(&mut verifier, &mut Cursor::new(&mut data)).unwrap();
                            }
                            rawzip::CompressionMethod::Store => {
                                let mut verifier = ent.verifying_reader(ent.data());
                                std::io::copy(&mut verifier, &mut Cursor::new(&mut data)).unwrap();
                            }
                            _ => todo!(
                                "Compression method not yet handled: {:?}",
                                entry.compression_method()
                            ),
                        }

                        match &expected_file.expected_content {
                            ExpectedContent::Content(expected_bytes) => {
                                assert_eq!(
                                    &data, expected_bytes,
                                    "Content mismatch for file {} in {}",
                                    expected_file.name, case.name
                                );
                            }
                            ExpectedContent::File(content_file_name) => {
                                let content_path = Path::new("assets").join(content_file_name);
                                let expected_bytes = std::fs::read(content_path).unwrap();
                                assert_eq!(
                                    &data, &expected_bytes,
                                    "Content mismatch for file {} (from {}) in {}",
                                    expected_file.name, content_file_name, case.name
                                );
                            }
                        }
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => panic!("Error iterating entries in {}: {:?}", case.name, e),
            }
        }
        if !found_file {
            panic!(
                "Expected file {} not found in archive {}",
                expected_file.name, case.name
            );
        }
    }
    assert_eq!(
        actual_files_found,
        case.files.len(),
        "File count mismatch for {}. Expected {}, found {}",
        case.name,
        case.files.len(),
        actual_files_found
    );

    Ok(())
}

fn run_zip_test_case_reader(case: &ZipTestCase) {
    let file_path = Path::new("assets").join(case.name);
    let f = File::open(file_path).unwrap();

    fn processor(f: File, case: &ZipTestCase) -> Result<(), Error> {
        let mut buf = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
        let archive = rawzip::ZipArchive::from_file(f, &mut buf[..])?;
        process_archive_files(&archive, case, &mut buf)?;
        Ok(())
    }

    match (processor(f, case), case.expected_error_kind.as_ref()) {
        (Ok(_), None) => {}
        (Ok(_), Some(expected)) => {
            panic!(
                "Expected error {:?}, but got Ok for {}",
                expected, case.name
            );
        }
        (Err(e), None) => {
            panic!("Unexpected error {:?} for {}", e, case.name);
        }
        (Err(e), Some(expected)) => {
            assert!(
                errors_eq(&e, expected),
                "Error kind mismatch for {}: {:?} != {:?}",
                case.name,
                e.kind(),
                expected
            );
        }
    };
}

fn run_zip_test_case_slice(case: &ZipTestCase) {
    fn processor(case: &ZipTestCase) -> Result<(), Error> {
        let file_path = Path::new("assets").join(case.name);
        let data = std::fs::read(file_path).unwrap();

        let archive = rawzip::ZipArchive::from_slice(data.as_slice())?;
        process_slice_archive_files(&archive, case)?;
        Ok(())
    }

    match (processor(case), case.expected_error_kind.as_ref()) {
        (Ok(_), None) => {}
        (Ok(_), Some(expected)) => {
            panic!(
                "Expected error {:?}, but got Ok for {}",
                expected, case.name
            );
        }
        (Err(e), None) => {
            panic!("Unexpected error {:?} for {}", e, case.name);
        }
        (Err(e), Some(expected)) => {
            assert!(
                errors_eq(&e, expected),
                "Error kind mismatch for {}: {:?} != {:?}",
                case.name,
                e.kind(),
                expected
            );
        }
    };
}

fn errors_eq(a: &Error, b: &ErrorKind) -> bool {
    match (a.kind(), b) {
        (
            ErrorKind::InvalidSignature {
                expected: a_exp, ..
            },
            ErrorKind::InvalidSignature {
                expected: b_exp, ..
            },
        ) => a_exp == b_exp,
        (
            ErrorKind::InvalidChecksum {
                expected: a_exp, ..
            },
            ErrorKind::InvalidChecksum {
                expected: b_exp, ..
            },
        ) => a_exp == b_exp,
        (
            ErrorKind::InvalidSize {
                expected: a_exp, ..
            },
            ErrorKind::InvalidSize {
                expected: b_exp, ..
            },
        ) => a_exp == b_exp,
        (ErrorKind::InvalidUtf8(a), ErrorKind::InvalidUtf8(b)) => a == b,
        (ErrorKind::InvalidInput { msg: a }, ErrorKind::InvalidInput { msg: b }) => a == b,
        (ErrorKind::IO(a), ErrorKind::IO(b)) => a.kind() == b.kind(),
        (ErrorKind::Eof, ErrorKind::Eof) => true,
        (ErrorKind::MissingEndOfCentralDirectory, ErrorKind::MissingEndOfCentralDirectory) => true,
        (
            ErrorKind::MissingZip64EndOfCentralDirectory,
            ErrorKind::MissingZip64EndOfCentralDirectory,
        ) => true,
        (ErrorKind::BufferTooSmall, ErrorKind::BufferTooSmall) => true,
        _ => false,
    }
}

/// This test is to ensure that the ZipArchive can be created from a Vec<u8>
#[test]
fn zip_integration_tests_vec() {
    let data = std::fs::read("assets/zip64.zip").unwrap();
    let archive = rawzip::ZipArchive::from_slice(data).unwrap();
    assert_eq!(archive.comment().as_bytes(), b"");
    let reader = archive.into_reader();
    let mut buf = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
    let mut entries = reader.entries(&mut buf);
    let mut count = 0;
    while let Some(entry) = entries.next_entry().unwrap() {
        if entry.is_dir() {
            continue;
        }
        count += 1;
    }
    assert_eq!(count, 1);
}

#[quickcheck]
fn test_read_what_we_write_slice(data: Vec<u8>) {
    let mut output = Vec::new();
    {
        let mut archive = rawzip::ZipArchiveWriter::new(&mut output);
        let (mut entry, config) = archive.new_file("file.txt").start().unwrap();
        let mut writer = config.wrap(&mut entry);
        std::io::copy(&mut Cursor::new(&data), &mut writer).unwrap();
        let (_, descriptor) = writer.finish().unwrap();
        assert_eq!(descriptor.uncompressed_size(), data.len() as u64);
        let compressed = entry.finish(descriptor).unwrap();
        assert_eq!(compressed, data.len() as u64);
        archive.finish().unwrap();
    }

    let archive = rawzip::ZipArchive::from_slice(&output).unwrap();
    let mut entries = archive.entries();
    let entry = entries.next_entry().unwrap().unwrap();
    assert_eq!(
        entry.file_path().try_normalize().unwrap().as_ref(),
        "file.txt"
    );
    assert_eq!(entry.compression_method(), rawzip::CompressionMethod::Store);
    assert_eq!(entry.uncompressed_size_hint(), data.len() as u64);
    assert_eq!(entry.compressed_size_hint(), data.len() as u64);
    let wayfinder = entry.wayfinder();
    let entry = archive.get_entry(wayfinder).unwrap();
    let mut actual = Vec::new();
    std::io::copy(&mut entry.data(), &mut Cursor::new(&mut actual)).unwrap();
    assert_eq!(data, actual);
}

#[test]
fn invalid_directory_offset_should_fail_to_parse() {
    let data = [
        80, 75, 5, 6, 255, 255, 6, 1, 250, 255, 255, 255, 255, 255, 255, 80, 75, 255, 255, 249,
        255, 255, 255, 255, 127, 255,
    ];
    let result = rawzip::ZipArchive::from_slice(&data);
    assert!(result.is_err());

    let mut buf = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
    let locator = rawzip::ZipLocator::new();
    let result = locator.locate_in_reader(&data[..], &mut buf, data.len() as u64);
    assert!(result.is_err());
}

#[test]
fn test_should_not_overflow_on_offsets() {
    let data = [
        80, 75, 3, 4, 20, 0, 0, 0, 8, 0, 48, 116, 10, 65, 126, 231, 255, 105, 36, 0, 0, 0, 36, 1,
        0, 0, 0, 0, 0, 0, 69, 219, 65, 68, 77, 69, 11, 201, 200, 44, 86, 40, 206, 77, 204, 201, 81,
        72, 203, 204, 73, 34, 0, 60, 242, 76, 243, 20, 48, 162, 204, 3, 20, 210, 242, 139, 114, 19,
        75, 244, 184, 0, 80, 75, 1, 2, 45, 3, 45, 0, 0, 0, 8, 0, 48, 114, 10, 65, 126, 231, 255,
        105, 255, 255, 255, 255, 255, 255, 255, 255, 6, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 164, 129,
        0, 0, 0, 0, 82, 69, 65, 68, 77, 69, 1, 0, 16, 0, 36, 16, 0, 0, 0, 80, 75, 5, 255, 255, 255,
        255, 255, 255, 255, 255, 80, 75, 6, 6, 44, 0, 0, 0, 0, 0, 0, 0, 45, 0, 45, 0, 0, 0, 0, 0,
        0, 255, 255, 255, 255, 255, 128, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 72, 0, 0, 0, 0, 0,
        0, 0, 72, 0, 0, 0, 0, 0, 0, 0, 80, 75, 6, 7, 0, 0, 0, 0, 144, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0,
        0, 80, 75, 5, 6, 0, 0, 0, 64, 255, 255, 0, 0, 8, 0, 53, 116, 10, 65, 126, 231, 0, 0, 0, 0,
        7, 0, 0, 0, 0, 144, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0,
    ];
    let result = rawzip::ZipArchive::from_slice(&data).unwrap();
    let entries = result.entries();
    let mut has_error = false;

    for entry in entries {
        let Ok(entry) = entry else {
            has_error = true;
            break;
        };
        has_error |= result.get_entry(entry.wayfinder()).is_err();
    }
    assert!(has_error);

    let mut buf = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
    let locator = rawzip::ZipLocator::new();
    let result = locator
        .locate_in_reader(&data[..], &mut buf, data.len() as u64)
        .unwrap();
    let mut entries = result.entries(&mut buf);
    let mut has_error = false;

    loop {
        let Ok(entry) = entries.next_entry() else {
            has_error = true;
            break;
        };
        let Some(entry) = entry else {
            break;
        };
        has_error |= result.get_entry(entry.wayfinder()).is_err();
    }
    assert!(has_error);
}

#[test]
fn test_java_jar_cafe_extra_field() {
    let file = std::fs::File::open("assets/test.jar").expect("Failed to open test.jar");
    let mut buf = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
    let archive = ZipArchive::from_file(file, &mut buf).expect("Failed to create ZipArchive");

    let mut entries = archive.entries(&mut buf);
    let entry = entries.next_entry().unwrap().unwrap();
    let mut extra_fields = entry.extra_fields();

    let mut found = false;
    for (field_id, _field_data) in extra_fields.by_ref() {
        if field_id == ExtraFieldId::JAVA_JAR {
            found = true;
            break;
        }
    }

    assert!(found, "Expected to find JAVA_JAR extra field (CAFE)");
    assert!(
        extra_fields.remaining_bytes().is_empty(),
        "No remaining bytes expected after consuming fields"
    );
}

#[test]
fn test_filename_mismatch_handling() {
    // Test the filename mismatch test fixture with ZipEntry (file-based)
    let file = std::fs::File::open("assets/filename_mismatch_test.zip")
        .expect("Failed to open filename_mismatch_test.zip");
    let mut buf = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
    let archive = ZipArchive::from_file(file, &mut buf).expect("Failed to create ZipArchive");

    let mut entries = archive.entries(&mut buf);
    let entry_header = entries.next_entry().unwrap().unwrap();

    // Central directory should show "malware.exe"
    assert_eq!(entry_header.file_path().as_ref(), b"malware.exe",);

    // Get the ZipEntry to access local_file_path
    let wayfinder = entry_header.wayfinder();
    let entry = archive.get_entry(wayfinder).unwrap();
    let mut local_buffer = vec![0u8; 512];
    let local_header = entry.local_header(&mut local_buffer).unwrap();
    assert_eq!(local_header.file_path().as_ref(), b"safe_file.txt");

    // Test slice version
    let data = std::fs::read("assets/filename_mismatch_test.zip").unwrap();
    let slice_archive = rawzip::ZipArchive::from_slice(data.as_slice()).unwrap();
    let mut slice_entries = slice_archive.entries();
    let slice_header = slice_entries.next_entry().unwrap().unwrap();
    assert_eq!(slice_header.file_path().as_ref(), b"malware.exe",);

    let slice_wayfinder = slice_header.wayfinder();
    let slice_entry = slice_archive.get_entry(slice_wayfinder).unwrap();
    let slice_local_filename = slice_entry.file_path();
    assert_eq!(slice_local_filename.as_ref(), b"safe_file.txt",);
}

#[test]
fn test_central_directory_offset_consistency() {
    let test_files = [
        "test.zip",
        "test-prefix.zip",
        "test-trailing-junk.zip",
        "unix.zip",
        "winxp.zip",
        "zip64-2.zip",
        "zip64.zip",
    ];

    for test_file in &test_files {
        let file_path = Path::new("assets").join(test_file);
        let data = std::fs::read(&file_path).unwrap();

        // Test with ZipSliceArchive
        let slice_archive = ZipArchive::from_slice(data.as_slice()).unwrap();
        let slice_entries: Vec<_> = slice_archive
            .entries()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        // Test with ZipArchive
        let file = File::open(&file_path).unwrap();
        let mut buf = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
        let file_archive = ZipArchive::from_file(file, &mut buf).unwrap();

        let mut entries_iter = file_archive.entries(&mut buf);
        for slice_entry in slice_entries.iter() {
            let file_entry = entries_iter.next_entry().unwrap().unwrap();
            assert_eq!(
                slice_entry.central_directory_offset(),
                file_entry.central_directory_offset(),
                "Central directory offset mismatch",
            );
        }
        assert!(
            entries_iter.next_entry().unwrap().is_none(),
            "More entries in file archive than in slice archive"
        );
    }
}
