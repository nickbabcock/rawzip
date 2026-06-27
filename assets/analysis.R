library(scales)
library(tidyverse)
library(readr)

df <- read_csv("./rawzip-benchmark-data.csv")

reader_names <- c("rawzip_slice", "rawzip_reader", "zip", "rc_zip", "async_zip")

df <- df %>%
  mutate(
    fn = `function`,
    throughput_millions = (throughput_num / 1e6) / (sample_measured_value / iteration_count / 1e9)
  )

# Consistent colors across every chart. The two rawzip APIs share a hue (two
# blues) to read as one library with two flavors
reader_colors <- c(
  rawzip_slice  = "#08519c",
  rawzip_reader = "#6baed6",
  zip           = "#737373",
  rc_zip        = "#969696",
  async_zip     = "#bdbdbd"
)

reader_labels <- c(
  rawzip_slice = "rawzip (slice)",
  rawzip_reader = "rawzip (reader)",
  zip = "zip",
  rc_zip = "rc_zip",
  async_zip = "async_zip"
)

# Compression-ratio scan: throughput by implementation.
compression_mean <- df %>%
  filter(group == "compression_ratio") %>%
  mutate(fn_factor = factor(fn, levels = reader_names)) %>%
  group_by(fn_factor) %>%
  summarise(mean_throughput = mean(throughput_millions), .groups = "drop") %>%
  mutate(fn_ordered = fct_reorder(fn_factor, mean_throughput, .desc = TRUE))

compression_p <- ggplot(
  compression_mean,
  aes(x = fn_ordered, y = mean_throughput, fill = fn_factor)
) +
  geom_col(width = 0.7) +
  geom_text(
    aes(label = comma(mean_throughput, accuracy = 0.1)),
    vjust = -0.4, size = 3.5
  ) +
  scale_fill_manual(values = reader_colors, guide = "none") +
  scale_x_discrete("Zip Reader Implementation", labels = reader_labels) +
  scale_y_continuous(
    "Throughput (M entries/s)",
    breaks = pretty_breaks(8),
    labels = comma_format(),
    expand = expansion(mult = c(0, 0.12))
  ) +
  theme_minimal() +
  theme(
    plot.title = element_text(size = 14, face = "bold"),
    plot.subtitle = element_text(size = 12),
    axis.title.x = element_text(margin = margin(t = 15)),
    axis.text.x = element_text(angle = 30, hjust = 1)
  ) +
  labs(
    title = "Computing a Zip Archive's Compression Ratio",
    subtitle = "100,000-entry archive · higher is better",
    caption = "Data from rawzip benchmark suite"
  )
print(compression_p)
ggsave("rawzip-compression-ratio-comparison.png", plot = compression_p, width = 8, height = 5, dpi = 150)

# Extracting entries: throughput (archives processed/sec), for one entry vs all.
extract_mean <- df %>%
  filter(group == "extract") %>%
  mutate(fn_factor = factor(fn, levels = reader_names)) %>%
  group_by(value, fn_factor) %>%
  summarise(archives_per_sec = mean(throughput_millions) * 1e6, .groups = "drop") %>%
  mutate(
    fn_ordered = fct_reorder(fn_factor, archives_per_sec, .desc = TRUE),
    position = factor(
      value,
      levels = c("first", "all"),
      labels = c("Extract first entry", "Extract all 100,000 entries")
    )
  )

extract_p <- ggplot(
  extract_mean,
  aes(x = fn_ordered, y = archives_per_sec, fill = fn_factor)
) +
  geom_col(width = 0.7) +
  geom_text(
    aes(label = label_number(accuracy = 0.1, scale_cut = cut_short_scale())(archives_per_sec)),
    vjust = -0.4, size = 3
  ) +
  facet_wrap(~position, scales = "free_y") +
  scale_fill_manual(values = reader_colors, guide = "none") +
  scale_x_discrete("Zip Reader Implementation", labels = reader_labels) +
  scale_y_continuous(
    "Archives processed / s",
    breaks = pretty_breaks(6),
    labels = label_number(scale_cut = cut_short_scale()),
    expand = expansion(mult = c(0, 0.15))
  ) +
  theme_minimal() +
  theme(
    plot.title = element_text(size = 14, face = "bold"),
    plot.subtitle = element_text(size = 12),
    axis.title.x = element_text(margin = margin(t = 15)),
    axis.text.x = element_text(angle = 30, hjust = 1),
    strip.text = element_text(size = 11, face = "bold")
  ) +
  labs(
    title = "Extracting Files From a Zip Archive",
    subtitle = "100,000-entry archive · higher is better",
    caption = "Data from rawzip benchmark suite"
  )
print(extract_p)
ggsave("rawzip-extract-comparison.png", plot = extract_p, width = 9, height = 5, dpi = 150)

# Write benchmarks: throughput by archive format and implementation.
write_impls <- c("rawzip", "zip", "async_zip")
write_df <- df %>%
  filter(group == "write") %>%
  mutate(
    feature_type = str_extract(fn, "^[^/]+"),
    impl_name = str_extract(fn, "(?<=/).*$"),
    feature_factor = factor(feature_type, levels = c("minimal", "extra_fields")),
    impl_factor = factor(impl_name, levels = write_impls)
  )

write_mean <- write_df %>%
  group_by(feature_factor, impl_factor) %>%
  summarise(mean_throughput = mean(throughput_millions), .groups = "drop")

write_colors <- c(
  rawzip = reader_colors[["rawzip_slice"]],
  zip = reader_colors[["zip"]],
  async_zip = reader_colors[["async_zip"]]
)

write_p <- ggplot(
  write_mean,
  aes(x = feature_factor, y = mean_throughput, fill = impl_factor)
) +
  geom_col(position = position_dodge(width = 0.7), width = 0.7) +
  geom_text(
    aes(label = comma(mean_throughput, accuracy = 1)),
    position = position_dodge(width = 0.7), vjust = -0.4, size = 3.5
  ) +
  scale_fill_manual(values = write_colors, name = "Implementation") +
  scale_y_continuous(
    "Throughput (MB/s)",
    breaks = pretty_breaks(8),
    labels = comma_format(),
    expand = expansion(mult = c(0, 0.12))
  ) +
  scale_x_discrete(
    "Zip Format Type",
    labels = c("minimal" = "Minimal Format", "extra_fields" = "With Extra Fields")
  ) +
  theme_minimal() +
  theme(
    plot.title = element_text(size = 14, face = "bold"),
    plot.subtitle = element_text(size = 12),
    axis.title.x = element_text(margin = margin(t = 15)),
    legend.position = "bottom"
  ) +
  labs(
    title = "Writing a Zip Archive",
    subtitle = "5,000-entry archive · higher is better",
    caption = "Data from rawzip benchmark suite"
  )
print(write_p)
ggsave("rawzip-write-performance-comparison.png", plot = write_p, width = 8, height = 5, dpi = 150)
