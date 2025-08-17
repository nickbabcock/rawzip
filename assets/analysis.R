library(scales)
library(tidyverse)
library(readr)
library(RColorBrewer)

df <- read_csv("./rawzip-benchmark-data.csv")

function_names <- c("rawzip", "async_zip", "rc_zip", "zip")

# Calculate throughput in MB/s (bytes per nanosecond * 1000 to get MB/s)
df <- df %>%
  mutate(
    fn = `function`,
    throughput_mbps = (throughput_num / 1e6) / (sample_measured_value / iteration_count / 1e9),
    is_rawzip = fn == "rawzip",
    # Create a factor for consistent ordering in legend
    fn_factor = factor(fn, levels = function_names)
  )

# Filter data for parse group
parse_df <- df %>% filter(group == "parse")

# Define colors for each zip reader using RColorBrewer Set1 palette
pal <- brewer.pal(4, "Set1")
colors <- setNames(pal, function_names)

# Calculate mean throughput by implementation for parse group
parse_mean_throughput <- parse_df %>%
  group_by(fn_factor) %>%
  summarise(mean_throughput_mbps = mean(throughput_mbps), .groups = 'drop')

# Parse performance graph
parse_p <- ggplot(parse_mean_throughput, aes(x = fn_factor, y = mean_throughput_mbps, fill = fn_factor)) +
  geom_col(width = 0.7) +
  # Color scale
  scale_fill_manual(values = colors, guide = "none") +
  # Axis formatting
  scale_y_continuous(
    "Throughput (MB/s)", 
    breaks = pretty_breaks(8),
    labels = comma_format()
  ) +
  scale_x_discrete("Zip Reader Implementation") +
  # Theme and labels
  theme_minimal() +
  theme(
    plot.title = element_text(size = 14, face = "bold"),
    plot.subtitle = element_text(size = 12),
    axis.title.x = element_text(margin = margin(t = 15))
  ) +
  labs(
    title = "Rust Zip Reader Performance Comparison",
    subtitle = "Mean central directory parsing throughput (higher is better)",
    caption = "Data from rawzip benchmark suite"
  )
print(parse_p)
ggsave('rawzip-performance-comparison.png', plot = parse_p, width = 8, height = 5, dpi = 150)

# Filter data for write group and prepare for hierarchical analysis
write_df <- df %>% 
  filter(group == "write") %>%
  mutate(
    # Extract feature type (extra_fields or minimal) and implementation
    feature_type = str_extract(fn, "^[^/]+"),
    impl_name = str_extract(fn, "(?<=/).*$"),
    # Create factors for consistent ordering
    feature_factor = factor(feature_type, levels = c("minimal", "extra_fields")),
    impl_factor = factor(impl_name, levels = c("rawzip", "zip", "async_zip"))
  )

# Calculate mean throughput by feature type and implementation
write_detailed_throughput <- write_df %>%
  group_by(feature_factor, impl_factor) %>%
  summarise(mean_throughput_mbps = mean(throughput_mbps), .groups = 'drop')

# Create a combined write performance graph
write_detailed_p <- ggplot(write_detailed_throughput, aes(x = feature_factor, y = mean_throughput_mbps, fill = impl_factor)) +
  geom_col(position = "dodge", width = 0.7) +
  # Color scale with rawzip and zip colors
  scale_fill_manual(values = c("rawzip" = colors[["rawzip"]], "zip" = colors[["zip"]], "async_zip" = colors[["async_zip"]]), 
                    name = "Implementation") +
  # Axis formatting
  scale_y_continuous(
    "Throughput (MB/s)", 
    breaks = pretty_breaks(8),
    labels = comma_format()
  ) +
  scale_x_discrete("Zip Format Type", 
                   labels = c("minimal" = "Minimal Format", "extra_fields" = "With Extra Fields")) +
  # Theme and labels
  theme_minimal() +
  theme(
    plot.title = element_text(size = 14, face = "bold"),
    plot.subtitle = element_text(size = 12),
    axis.title.x = element_text(margin = margin(t = 15)),
    legend.position = "bottom"
  ) +
  labs(
    title = "Zip Writer Performance",
    subtitle = "Mean writing throughput by format type and implementation (higher is better)",
    caption = "Data from rawzip benchmark suite"
  )
print(write_detailed_p)
ggsave('rawzip-write-performance-comparison.png', plot = write_detailed_p, width = 8, height = 5, dpi = 150)
