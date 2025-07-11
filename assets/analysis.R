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

# Define colors for each zip reader using RColorBrewer Set1 palette
pal <- brewer.pal(4, "Set1")
colors <- setNames(pal, function_names)

# Calculate mean throughput by implementation
mean_throughput <- df %>%
  group_by(fn_factor) %>%
  summarise(mean_throughput_mbps = mean(throughput_mbps), .groups = 'drop')

p <- ggplot(mean_throughput, aes(x = fn_factor, y = mean_throughput_mbps, fill = fn_factor)) +
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
print(p)
ggsave('rawzip-performance-comparison.png', plot = p, width = 8, height = 5, dpi = 150)
