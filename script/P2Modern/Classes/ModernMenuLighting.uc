class ModernMenuLighting extends ModernMenuPage;

function CreateMenuContents()
{
	local UWindowComboControl Combo;

	Super.CreateMenuContents();

	AddTitle(TitleText, TitleFont, TitleAlign);

	bUpdate = false;
	Combo = AddLevelCombo(0, "ModernSunShadows", 3, 4);
	AddNames(Combo, 0, 4);
	AddLevelSlider(1, "ModernShadowDistance", 1, 5, 4);
	Combo = AddLevelCombo(2, "ModernLampShadows", 3, 4);
	AddNames(Combo, 0, 4);
	Combo = AddLevelCombo(3, "ModernContactShadows", 1, 2);
	AddNames(Combo, 0, 1);
	Combo.AddItem(LevelNames[3]);
	Combo = AddLevelCombo(4, "ModernAO", 2, 3);
	AddNames(Combo, 0, 3);
	Combo = AddLevelCombo(5, "ModernIndirect", 2, 2);
	AddNames(Combo, 0, 1);
	Combo.AddItem(LevelNames[3]);
	AddBox(6, "ModernTerrainDetail");
	AddBack();
	LoadRows();
}

defaultproperties
{
	TitleText="Shadows and Lighting"
	BackHelp="Return to the Modern Graphics menu."

	Labels(0)="Sun shadows"
	Labels(1)="Shadow distance"
	Labels(2)="Lamp shadows"
	Labels(3)="Contact shadows"
	Labels(4)="Ambient occlusion"
	Labels(5)="Indirect lighting"
	Labels(6)="Terrain tessellation"

	Helps(0)="Sun shadows from characters and moving objects; higher is sharper."
	Helps(1)="How far away sun shadows are drawn."
	Helps(2)="Shadows from lamps and other lights in the level."
	Helps(3)="Small sharp shadows where objects touch the ground and walls."
	Helps(4)="Darkens corners, creases and gaps; turns MSAA off."
	Helps(5)="Light bounced around the level brightens shaded areas."
	Helps(6)="Hills and slopes look rounded instead of faceted; applies when a level loads."
}
