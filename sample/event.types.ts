export type Json =
  | string
  | number
  | boolean
  | null
  | { [key: string]: Json | undefined }
  | Json[]

export type Database = {
  // Allows to automatically instantiate createClient with right options
  // instead of createClient<Database, { PostgrestVersion: 'XX' }>(URL, KEY)
  __InternalSupabase: {
    PostgrestVersion: "12"
  }
  public: {
    Tables: {
      area: {
        Row: {
          capacity: number | null
          id: string
          kind: string
          name: string
          parent_id: string | null
        }
        Insert: {
          capacity?: number | null
          id?: string
          kind: string
          name: string
          parent_id?: string | null
        }
        Update: {
          capacity?: number | null
          id?: string
          kind?: string
          name?: string
          parent_id?: string | null
        }
        Relationships: [
          {
            foreignKeyName: "area_parent_id_fkey"
            columns: ["parent_id"]
            isOneToOne: false
            referencedRelation: "area"
            referencedColumns: ["id"]
          },
        ]
      }
      artist: {
        Row: {
          country: string | null
          genres: Json
          id: string
          name: string
        }
        Insert: {
          country?: string | null
          genres?: Json
          id?: string
          name: string
        }
        Update: {
          country?: string | null
          genres?: Json
          id?: string
          name?: string
        }
        Relationships: []
      }
      event: {
        Row: {
          area_id: string
          ends_on: string
          id: string
          is_public: number
          name: string
          starts_on: string
        }
        Insert: {
          area_id: string
          ends_on: string
          id?: string
          is_public?: number
          name: string
          starts_on: string
        }
        Update: {
          area_id?: string
          ends_on?: string
          id?: string
          is_public?: number
          name?: string
          starts_on?: string
        }
        Relationships: [
          {
            foreignKeyName: "event_area_id_fkey"
            columns: ["area_id"]
            isOneToOne: false
            referencedRelation: "area"
            referencedColumns: ["id"]
          },
        ]
      }
      lineup: {
        Row: {
          artist_id: string
          billing_order: number
          role: string
          sub_event_id: string
        }
        Insert: {
          artist_id: string
          billing_order?: number
          role?: string
          sub_event_id: string
        }
        Update: {
          artist_id?: string
          billing_order?: number
          role?: string
          sub_event_id?: string
        }
        Relationships: [
          {
            foreignKeyName: "lineup_artist_id_fkey"
            columns: ["artist_id"]
            isOneToOne: false
            referencedRelation: "artist"
            referencedColumns: ["id"]
          },
          {
            foreignKeyName: "lineup_sub_event_id_fkey"
            columns: ["sub_event_id"]
            isOneToOne: false
            referencedRelation: "sub_event"
            referencedColumns: ["id"]
          },
        ]
      }
      sub_event: {
        Row: {
          area_id: string | null
          ends_at: string
          event_id: string
          id: string
          price: number | null
          starts_at: string
          title: string
        }
        Insert: {
          area_id?: string | null
          ends_at: string
          event_id: string
          id?: string
          price?: number | null
          starts_at: string
          title: string
        }
        Update: {
          area_id?: string | null
          ends_at?: string
          event_id?: string
          id?: string
          price?: number | null
          starts_at?: string
          title?: string
        }
        Relationships: [
          {
            foreignKeyName: "sub_event_area_id_fkey"
            columns: ["area_id"]
            isOneToOne: false
            referencedRelation: "area"
            referencedColumns: ["id"]
          },
          {
            foreignKeyName: "sub_event_event_id_fkey"
            columns: ["event_id"]
            isOneToOne: false
            referencedRelation: "event"
            referencedColumns: ["id"]
          },
        ]
      }
    }
    Views: {
      [_ in never]: never
    }
    Functions: {
      [_ in never]: never
    }
    Enums: {
      [_ in never]: never
    }
    CompositeTypes: {
      [_ in never]: never
    }
  }
}

type DatabaseWithoutInternals = Omit<Database, "__InternalSupabase">

type DefaultSchema = DatabaseWithoutInternals[Extract<keyof Database, "public">]

export type Tables<
  DefaultSchemaTableNameOrOptions extends
    | keyof (DefaultSchema["Tables"] & DefaultSchema["Views"])
    | { schema: keyof DatabaseWithoutInternals },
  TableName extends DefaultSchemaTableNameOrOptions extends {
    schema: keyof DatabaseWithoutInternals
  }
    ? keyof (DatabaseWithoutInternals[DefaultSchemaTableNameOrOptions["schema"]]["Tables"] &
        DatabaseWithoutInternals[DefaultSchemaTableNameOrOptions["schema"]]["Views"])
    : never = never,
> = DefaultSchemaTableNameOrOptions extends {
  schema: keyof DatabaseWithoutInternals
}
  ? (DatabaseWithoutInternals[DefaultSchemaTableNameOrOptions["schema"]]["Tables"] &
      DatabaseWithoutInternals[DefaultSchemaTableNameOrOptions["schema"]]["Views"])[TableName] extends {
      Row: infer R
    }
    ? R
    : never
  : DefaultSchemaTableNameOrOptions extends keyof (DefaultSchema["Tables"] &
        DefaultSchema["Views"])
    ? (DefaultSchema["Tables"] &
        DefaultSchema["Views"])[DefaultSchemaTableNameOrOptions] extends {
        Row: infer R
      }
      ? R
      : never
    : never

export type TablesInsert<
  DefaultSchemaTableNameOrOptions extends
    | keyof DefaultSchema["Tables"]
    | { schema: keyof DatabaseWithoutInternals },
  TableName extends DefaultSchemaTableNameOrOptions extends {
    schema: keyof DatabaseWithoutInternals
  }
    ? keyof DatabaseWithoutInternals[DefaultSchemaTableNameOrOptions["schema"]]["Tables"]
    : never = never,
> = DefaultSchemaTableNameOrOptions extends {
  schema: keyof DatabaseWithoutInternals
}
  ? DatabaseWithoutInternals[DefaultSchemaTableNameOrOptions["schema"]]["Tables"][TableName] extends {
      Insert: infer I
    }
    ? I
    : never
  : DefaultSchemaTableNameOrOptions extends keyof DefaultSchema["Tables"]
    ? DefaultSchema["Tables"][DefaultSchemaTableNameOrOptions] extends {
        Insert: infer I
      }
      ? I
      : never
    : never

export type TablesUpdate<
  DefaultSchemaTableNameOrOptions extends
    | keyof DefaultSchema["Tables"]
    | { schema: keyof DatabaseWithoutInternals },
  TableName extends DefaultSchemaTableNameOrOptions extends {
    schema: keyof DatabaseWithoutInternals
  }
    ? keyof DatabaseWithoutInternals[DefaultSchemaTableNameOrOptions["schema"]]["Tables"]
    : never = never,
> = DefaultSchemaTableNameOrOptions extends {
  schema: keyof DatabaseWithoutInternals
}
  ? DatabaseWithoutInternals[DefaultSchemaTableNameOrOptions["schema"]]["Tables"][TableName] extends {
      Update: infer U
    }
    ? U
    : never
  : DefaultSchemaTableNameOrOptions extends keyof DefaultSchema["Tables"]
    ? DefaultSchema["Tables"][DefaultSchemaTableNameOrOptions] extends {
        Update: infer U
      }
      ? U
      : never
    : never

export type Enums<
  DefaultSchemaEnumNameOrOptions extends
    | keyof DefaultSchema["Enums"]
    | { schema: keyof DatabaseWithoutInternals },
  EnumName extends DefaultSchemaEnumNameOrOptions extends {
    schema: keyof DatabaseWithoutInternals
  }
    ? keyof DatabaseWithoutInternals[DefaultSchemaEnumNameOrOptions["schema"]]["Enums"]
    : never = never,
> = DefaultSchemaEnumNameOrOptions extends {
  schema: keyof DatabaseWithoutInternals
}
  ? DatabaseWithoutInternals[DefaultSchemaEnumNameOrOptions["schema"]]["Enums"][EnumName]
  : DefaultSchemaEnumNameOrOptions extends keyof DefaultSchema["Enums"]
    ? DefaultSchema["Enums"][DefaultSchemaEnumNameOrOptions]
    : never

export type CompositeTypes<
  PublicCompositeTypeNameOrOptions extends
    | keyof DefaultSchema["CompositeTypes"]
    | { schema: keyof DatabaseWithoutInternals },
  CompositeTypeName extends PublicCompositeTypeNameOrOptions extends {
    schema: keyof DatabaseWithoutInternals
  }
    ? keyof DatabaseWithoutInternals[PublicCompositeTypeNameOrOptions["schema"]]["CompositeTypes"]
    : never = never,
> = PublicCompositeTypeNameOrOptions extends {
  schema: keyof DatabaseWithoutInternals
}
  ? DatabaseWithoutInternals[PublicCompositeTypeNameOrOptions["schema"]]["CompositeTypes"][CompositeTypeName]
  : PublicCompositeTypeNameOrOptions extends keyof DefaultSchema["CompositeTypes"]
    ? DefaultSchema["CompositeTypes"][PublicCompositeTypeNameOrOptions]
    : never

export const Constants = {
  public: {
    Enums: {},
  },
} as const
